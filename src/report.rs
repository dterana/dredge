//! Human and JSON rendering. Neither renderer has access to secret values:
//! findings carry only rule id, path and line.
//!
//! Empty-section rule (human output): every core section header is always
//! printed, followed by `  (none)` when it has no entries. Sections that only
//! exist with `--all` (TRACKED, CACHE/STATE) follow the same rule when shown.
//! The closing "in repo, missing on this machine" note is a note, not a
//! section, and is printed only when it has entries. Past `MISSING_CAP`
//! entries it shows the count and, without `--all`, only the first ones.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;
use serde_json::{json, Value};

use crate::layout::Layout;
use crate::model::{Adopt, Finding, Kind, Report, Tracking, Unit};
use crate::secrets::MAX_SCAN_BYTES;
use crate::walk::MAX_DEPTH;

/// Unknown units shown without `--all`.
pub const UNKNOWN_CAP: usize = 20;

/// "In repo, missing on this machine" entries shown without `--all`.
pub const MISSING_CAP: usize = 20;

/// Adopt commands shown without `--all`.
const ADOPT_CAP: usize = 20;

const APP_W: usize = 14;
const PATH_W: usize = 44;

/// What the ignore list was read from.
fn ignore_names(report: &Report) -> &'static str {
    if report.chezmoi_ignore {
        ".dredgeignore or .chezmoiignore"
    } else {
        ".dredgeignore"
    }
}

/// A short label for each dotfiles directory: its own name, or when two share
/// a name, enough of the path above it to tell them apart (`work/dotfiles`
/// and `personal/dotfiles`).
fn dotfiles_labels(dirs: &[PathBuf]) -> Vec<String> {
    let comps = |d: &PathBuf, k: usize| -> String {
        let parts: Vec<String> = d
            .components()
            .rev()
            .take(k)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        parts.into_iter().rev().collect::<Vec<_>>().join("/")
    };
    dirs.iter()
        .map(|d| {
            let depth = d.components().count().max(1);
            for k in 1..=depth {
                let mine = comps(d, k);
                if dirs.iter().filter(|o| *o != d).all(|o| comps(o, k) != mine) {
                    return mine;
                }
            }
            d.to_string_lossy().into_owned()
        })
        .collect()
}

/// Display a path: `~/...` under home, `<repo dir label>/...` under a
/// dotfiles dir (the first listed one that contains it), else absolute.
/// Lossy, for display only.
pub fn display_path(report: &Report, p: &Path) -> String {
    if !report.home.as_os_str().is_empty() {
        if let Ok(rel) = p.strip_prefix(&report.home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.to_string_lossy());
        }
    }
    let labels = dotfiles_labels(&report.dotfiles);
    for (d, name) in report.dotfiles.iter().zip(labels) {
        if let Ok(rel) = p.strip_prefix(d) {
            if rel.as_os_str().is_empty() {
                return name;
            }
            return format!("{name}/{}", rel.to_string_lossy());
        }
    }
    p.to_string_lossy().into_owned()
}

/// Home-relative folder whose unknown units the human report collapses.
const APP_SUPPORT: &str = "Library/Application Support";

/// Home-relative folder of preference files, collapsed the same way.
const PREFERENCES: &str = "Library/Preferences";

/// Folders whose unknown units collapse into one line each, with the noun
/// for one and for several.
const COLLAPSED_ROOTS: [(&str, &str, &str); 2] = [
    (APP_SUPPORT, "app", "apps"),
    (PREFERENCES, "preference file", "preference files"),
];

/// Home-relative folder holding the macOS privacy-protected folders that
/// the human report summarizes under COULD NOT READ.
const LIBRARY: &str = "Library";

/// The index in `COLLAPSED_ROOTS` of the folder a path is strictly inside.
fn collapsed_root(report: &Report, p: &Path) -> Option<usize> {
    let rel = p.strip_prefix(&report.home).ok()?;
    COLLAPSED_ROOTS
        .iter()
        .position(|(root, _, _)| rel.starts_with(root) && rel != Path::new(root))
}

/// True for a path strictly inside `~/Library`.
fn in_library(report: &Report, p: &Path) -> bool {
    p.strip_prefix(&report.home)
        .is_ok_and(|rel| rel.starts_with(LIBRARY) && rel != Path::new(LIBRARY))
}

/// A path as a shell word: `~/rel` when the name is plain, else a double
/// quoted `"$HOME/rel"` with the characters the shell would act on escaped.
/// Paths outside home are written absolute, quoted the same way.
fn shell_word(report: &Report, p: &Path) -> String {
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./+@%:,=-".contains(c))
    };
    let quote = |s: &str| {
        let mut o = String::from("\"");
        for c in s.chars() {
            if matches!(c, '"' | '\\' | '`' | '$') {
                o.push('\\');
            }
            o.push(c);
        }
        o.push('"');
        o
    };
    match p.strip_prefix(&report.home) {
        Ok(rel) if !report.home.as_os_str().is_empty() => {
            let rel = rel.to_string_lossy();
            if plain(&rel) {
                format!("~/{rel}")
            } else {
                let esc = quote(&rel);
                format!("\"$HOME/{}\"", &esc[1..esc.len() - 1])
            }
        }
        _ => {
            let s = p.to_string_lossy();
            if plain(&s) {
                s.into_owned()
            } else {
                quote(&s)
            }
        }
    }
}

/// Stow package name for a unit: the app name, else its file name without
/// leading dots.
fn stow_package(rel: &Path, app: Option<&str>) -> String {
    let raw = app.map(str::to_string).unwrap_or_else(|| {
        rel.file_name()
            .map(|n| n.to_string_lossy().trim_start_matches('.').to_string())
            .unwrap_or_default()
    });
    let name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "misc".to_string()
    } else {
        name
    }
}

/// One adopt command per untracked config unit, capped like the other lists.
fn adopt_lines(report: &Report, adopt: &Adopt, all: bool) -> Vec<String> {
    let units: Vec<&Unit> = report
        .units
        .iter()
        .filter(|u| u.kind == Kind::Config && is_untracked(u.tracking) && !u.ignored && !u.symlink)
        .filter(|u| u.path.strip_prefix(&report.home).is_ok())
        .collect();
    let shown = if all {
        units.len()
    } else {
        units.len().min(ADOPT_CAP)
    };
    let dir = shell_word(report, &adopt.dir);
    let home = match shell_word(report, &report.home).as_str() {
        "~/" => "~".to_string(),
        w => w.to_string(),
    };
    let mut lines = Vec::new();
    for u in &units[..shown] {
        let rel = u.path.strip_prefix(&report.home).unwrap_or(&u.path);
        let src = shell_word(report, &u.path);
        if !u.findings.is_empty() {
            lines.push(match adopt.layout {
                Layout::Chezmoi => format!("  chezmoi add --encrypt {src}   # holds a secret"),
                Layout::Stow => format!(
                    "  # {} holds a secret: keep it out of the repo, or encrypt it first",
                    display_path(report, &u.path)
                ),
            });
            continue;
        }
        lines.push(match adopt.layout {
            Layout::Chezmoi => format!("  chezmoi add {src}"),
            Layout::Stow => {
                let pkg = stow_package(rel, u.app.as_deref());
                let parent = rel.parent().filter(|p| !p.as_os_str().is_empty());
                let pkg_dir = match parent {
                    Some(par) => shell_word(report, &adopt.dir.join(&pkg).join(par)),
                    None => shell_word(report, &adopt.dir.join(&pkg)),
                };
                format!(
                    "  mkdir -p {pkg_dir} && mv {src} {pkg_dir}/ && stow -d {dir} -t {home} {pkg}"
                )
            }
        });
    }
    if shown < units.len() {
        lines.push(format!(
            "  (+ {} more, run with --all to list)",
            thousands(units.len() - shown)
        ));
    }
    lines
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 1204 -> "1,204".
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[derive(Clone, Copy)]
enum Tone {
    Header,
    Danger,
    Warn,
    Dim,
}

struct Painter {
    color: bool,
}

impl Painter {
    fn paint(&self, tone: Tone, s: &str) -> String {
        if !self.color {
            return s.to_string();
        }
        match tone {
            Tone::Header => s.bold().to_string(),
            Tone::Danger => s.red().bold().to_string(),
            Tone::Warn => s.yellow().to_string(),
            Tone::Dim => s.dimmed().to_string(),
        }
    }
}

fn is_tracked(t: Tracking) -> bool {
    matches!(
        t,
        Tracking::Tracked | Tracking::TrackedDrifted | Tracking::TrackedUnverified
    )
}

fn is_untracked(t: Tracking) -> bool {
    matches!(t, Tracking::Untracked | Tracking::PartiallyTracked)
}

fn unknown_rank(u: &Unit) -> u8 {
    if !u.findings.is_empty() {
        0
    } else if u.tracking == Tracking::PartiallyTracked {
        1
    } else {
        2
    }
}

fn app_col(u: &Unit) -> String {
    format!("{:<APP_W$}", u.app.as_deref().unwrap_or("-"))
}

fn unit_line(report: &Report, p: &Painter, u: &Unit, tags: &[&str]) -> String {
    let path = display_path(report, &u.path);
    if tags.is_empty() {
        format!("  {} {}", app_col(u), path)
    } else {
        let tags = p.paint(Tone::Dim, &tags.join(", "));
        format!("  {} {:<PATH_W$} {}", app_col(u), path, tags)
    }
}

fn finding_text(f: &Finding) -> String {
    format!("rule: {}, line {}", f.rule_id, f.line)
}

/// Findings with the same rule, path and line folded into one row with a
/// count, in order of first appearance. Linear time: each key maps to its
/// row. Human output only.
fn collapse<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Vec<(&'a Finding, usize)> {
    let mut rows: Vec<(&Finding, usize)> = Vec::new();
    let mut index: HashMap<(&str, &Path, usize), usize> = HashMap::new();
    for f in findings {
        match index.entry((f.rule_id.as_str(), f.path.as_path(), f.line)) {
            Entry::Occupied(e) => {
                if let Some(row) = rows.get_mut(*e.get()) {
                    row.1 += 1;
                }
            }
            Entry::Vacant(e) => {
                e.insert(rows.len());
                rows.push((f, 1));
            }
        }
    }
    rows
}

/// The `  (xN)` marker for a collapsed row, empty for a single finding.
fn count_marker(p: &Painter, n: usize) -> String {
    if n > 1 {
        p.paint(Tone::Dim, &format!("  (x{n})"))
    } else {
        String::new()
    }
}

struct Out {
    buf: String,
}

impl Out {
    fn section(&mut self, header: &str, lines: &[String]) {
        if !self.buf.is_empty() {
            self.buf.push('\n');
        }
        self.buf.push_str(header);
        self.buf.push('\n');
        if lines.is_empty() {
            self.buf.push_str("  (none)\n");
        }
        for l in lines {
            self.buf.push_str(l);
            self.buf.push('\n');
        }
    }
}

pub fn render_human(report: &Report, color: bool, all: bool) -> String {
    let p = Painter { color };
    let mut out = Out { buf: String::new() };
    let h = |s: &str, danger: bool| {
        if danger {
            p.paint(Tone::Danger, s)
        } else {
            p.paint(Tone::Header, s)
        }
    };

    // COMMITTED SECRETS
    let lines: Vec<String> = collapse(&report.committed_findings)
        .into_iter()
        .map(|(f, n)| {
            format!(
                "  {:<w$} {}{}",
                display_path(report, &f.path),
                p.paint(Tone::Danger, &format!("SECRET  {}", finding_text(f))),
                count_marker(&p, n),
                w = APP_W + 1 + PATH_W
            )
        })
        .collect();
    out.section(&h("COMMITTED SECRETS", !lines.is_empty()), &lines);

    // UNTRACKED SECRETS: every finding on this machine. Findings inside a
    // tracked unit or a `.dredgeignore`d unit are tagged, never hidden.
    let mut lines = Vec::new();
    for u in &report.units {
        for (f, n) in collapse(&u.findings) {
            let mut line = format!(
                "  {} {:<PATH_W$} {}",
                app_col(u),
                display_path(report, &f.path),
                p.paint(Tone::Danger, &format!("SECRET  {}", finding_text(f)))
            );
            if u.ignored {
                line.push_str(&p.paint(
                    Tone::Dim,
                    &format!("  (ignored by {})", ignore_names(report)),
                ));
            } else if is_tracked(u.tracking) {
                line.push_str(&p.paint(Tone::Dim, "  (in a tracked unit)"));
            }
            line.push_str(&count_marker(&p, n));
            lines.push(line);
        }
    }
    out.section(&h("UNTRACKED SECRETS", !lines.is_empty()), &lines);

    // UNTRACKED CONFIG
    let lines: Vec<String> = report
        .units
        .iter()
        .filter(|u| u.kind == Kind::Config && is_untracked(u.tracking) && !u.ignored)
        .map(|u| {
            let mut tags = Vec::new();
            if u.tracking == Tracking::PartiallyTracked {
                tags.push("partially tracked");
            }
            if u.symlink {
                tags.push("symlink, not followed");
            }
            if !u.findings.is_empty() {
                tags.push("SECRET");
            }
            unit_line(report, &p, u, &tags)
        })
        .collect();
    out.section(&h("UNTRACKED CONFIG", false), &lines);

    // HOW TO ADOPT (only with --hints): commands to run by hand.
    if let Some(adopt) = &report.adopt {
        let lines = adopt_lines(report, adopt, all);
        out.section(
            &h(
                "HOW TO ADOPT (run these yourself, dredge never does)",
                false,
            ),
            &lines,
        );
    }

    // TRACKED, DRIFTED: one line per drifted file.
    let mut lines = Vec::new();
    for u in &report.units {
        for d in &u.drifted {
            lines.push(format!(
                "  {} {:<PATH_W$} {}",
                app_col(u),
                display_path(report, &d.path),
                p.paint(
                    Tone::Warn,
                    &format!("differs from {}", display_path(report, &d.repo_path))
                )
            ));
        }
    }
    out.section(&h("TRACKED, DRIFTED", false), &lines);

    if all {
        let lines: Vec<String> = report
            .units
            .iter()
            .filter(|u| matches!(u.kind, Kind::Config | Kind::Unknown) && is_tracked(u.tracking))
            .map(|u| {
                let mut tags = Vec::new();
                match u.tracking {
                    Tracking::TrackedDrifted => tags.push("drifted"),
                    Tracking::TrackedUnverified => tags.push("unverified"),
                    _ => {}
                }
                if u.symlink {
                    tags.push("symlink");
                }
                unit_line(report, &p, u, &tags)
            })
            .collect();
        out.section(&h("TRACKED", false), &lines);
    }

    // UNKNOWN. Without `--all`, unknown units under Application Support and
    // Preferences that carry no finding are counted on one line per folder
    // instead of listed; a unit with a finding is always listed.
    let mut unknown: Vec<&Unit> = report
        .units
        .iter()
        .filter(|u| u.kind == Kind::Unknown && is_untracked(u.tracking) && !u.ignored)
        .collect();
    let mut collapsed = [0usize; COLLAPSED_ROOTS.len()];
    if !all {
        unknown.retain(|u| match collapsed_root(report, &u.path) {
            Some(i) if u.findings.is_empty() => {
                collapsed[i] += 1;
                false
            }
            _ => true,
        });
    }
    // Most useful rows first so they survive the cap: findings, then
    // partially tracked, then the rest. Stable, so ties keep path order.
    unknown.sort_by_key(|u| unknown_rank(u));
    let shown = if all {
        unknown.len()
    } else {
        unknown.len().min(UNKNOWN_CAP)
    };
    let mut header = format!("UNKNOWN ({})", thousands(unknown.len()));
    if shown < unknown.len() {
        let _ = write!(header, "  showing {shown}, run with --all to list");
    }
    let mut lines: Vec<String> = unknown[..shown]
        .iter()
        .map(|u| {
            let mut tags = Vec::new();
            if u.tracking == Tracking::PartiallyTracked {
                tags.push("partially tracked");
            }
            if u.symlink {
                tags.push("symlink, not followed");
            }
            if !u.findings.is_empty() {
                tags.push("SECRET");
            }
            unit_line(report, &p, u, &tags)
        })
        .collect();
    for (&n, (root, one, many)) in collapsed.iter().zip(COLLAPSED_ROOTS) {
        if n > 0 {
            let noun = if n == 1 { one } else { many };
            lines.push(format!(
                "  (+ {} unclassified {noun} under ~/{root}, run with --all to list)",
                thousands(n)
            ));
        }
    }
    out.section(&h(&header, false), &lines);

    // COULD NOT READ. Without `--all`, the folders under ~/Library are one
    // line: macOS privacy protection blocks many of them (Mail, Messages,
    // Safari, Contacts) until the terminal has Full Disk Access.
    let (library, rest): (Vec<&PathBuf>, Vec<&PathBuf>) = if all {
        (Vec::new(), report.unreadable.iter().collect())
    } else {
        report
            .unreadable
            .iter()
            .partition(|u| in_library(report, u))
    };
    let mut lines: Vec<String> = rest
        .iter()
        .map(|u| format!("  {}", display_path(report, u)))
        .collect();
    if !library.is_empty() {
        let noun = if library.len() == 1 {
            "folder"
        } else {
            "folders"
        };
        lines.push(format!(
            "  ({} {noun} under ~/{LIBRARY}, probably protected by macOS: give your terminal Full Disk Access to scan {}, or run with --all to list)",
            thousands(library.len()),
            if library.len() == 1 { "it" } else { "them" }
        ));
    }
    out.section(&h("COULD NOT READ", false), &lines);

    // IGNORED summary
    out.buf.push('\n');
    let _ = writeln!(
        out.buf,
        "{}  {} cache/state items, {} ignored by {}",
        h("IGNORED", false),
        thousands(report.cache_state_count),
        thousands(report.ignored_by_file),
        ignore_names(report)
    );
    if report.pruned_dirs > 0 {
        let n = report.pruned_dirs;
        let (noun, verb) = if n == 1 {
            ("directory", "was")
        } else {
            ("directories", "were")
        };
        let _ = writeln!(
            out.buf,
            "  {} cache/state {noun} {verb} not entered",
            thousands(n)
        );
    }
    if report.depth_truncated > 0 {
        let n = report.depth_truncated;
        let (noun, verb) = if n == 1 {
            ("directory", "was")
        } else {
            ("directories", "were")
        };
        let _ = writeln!(
            out.buf,
            "  {} {noun} deeper than {MAX_DEPTH} levels {verb} not scanned",
            thousands(n)
        );
    }
    if report.watched_scanned > 0 {
        let n = report.watched_scanned;
        let noun = if n == 1 { "path" } else { "paths" };
        let _ = writeln!(
            out.buf,
            "  {} {noun} force-scanned with --watch",
            thousands(n)
        );
    }
    if report.skipped_too_large > 0 {
        let n = report.skipped_too_large;
        let (noun, verb) = if n == 1 {
            ("file", "was")
        } else {
            ("files", "were")
        };
        let _ = writeln!(
            out.buf,
            "  {} {noun} over {} MiB {verb} not scanned for secrets",
            thousands(n),
            MAX_SCAN_BYTES / (1024 * 1024)
        );
    }

    if all {
        let lines: Vec<String> = report
            .units
            .iter()
            .filter(|u| matches!(u.kind, Kind::Cache | Kind::State))
            .map(|u| unit_line(report, &p, u, &[u.kind.as_str()]))
            .collect();
        out.section(&h("CACHE/STATE", false), &lines);
    }

    if !report.missing_on_machine.is_empty() {
        let missing = &report.missing_on_machine;
        let shown = if all {
            missing.len()
        } else {
            missing.len().min(MISSING_CAP)
        };
        // Up to the cap the header carries no count, as it always did.
        let mut header = String::from("IN REPO, MISSING ON THIS MACHINE");
        if missing.len() > MISSING_CAP {
            let _ = write!(header, " ({})", thousands(missing.len()));
        }
        if shown < missing.len() {
            let _ = write!(header, "  showing {shown}, run with --all to list");
        }
        let lines: Vec<String> = missing[..shown]
            .iter()
            .map(|rel| format!("  {}", display_path(report, &report.home.join(rel))))
            .collect();
        out.section(&h(&header, false), &lines);
    }
    out.buf
}

fn finding_json(f: &Finding) -> Value {
    json!({
        "rule_id": f.rule_id,
        "path": lossy(&f.path),
        "line": f.line,
        "committed": f.committed,
    })
}

/// Schema version 1. Paths are lossy strings so non-UTF-8 names never make
/// serialization fail.
pub fn render_json(report: &Report) -> String {
    let units: Vec<Value> = report
        .units
        .iter()
        .map(|u| {
            json!({
                "path": lossy(&u.path),
                "app": u.app,
                "kind": u.kind.as_str(),
                "tracking": u.tracking.as_str(),
                "symlink": u.symlink,
                "ignored": u.ignored,
                "findings": u.findings.iter().map(finding_json).collect::<Vec<_>>(),
                "drifted": u.drifted.iter().map(|d| json!({
                    "path": lossy(&d.path),
                    "repo_path": lossy(&d.repo_path),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let value = json!({
        "schema_version": 1,
        "home": lossy(&report.home),
        "dotfiles": report.dotfiles.iter().map(|d| lossy(d)).collect::<Vec<_>>(),
        "committed_secrets": report.committed_findings.iter().map(finding_json).collect::<Vec<_>>(),
        "units": units,
        "unreadable": report.unreadable.iter().map(|p| lossy(p)).collect::<Vec<_>>(),
        "missing_on_machine": report.missing_on_machine.iter().map(|p| lossy(&report.home.join(p))).collect::<Vec<_>>(),
        "ignored": {
            "cache_state_items": report.cache_state_count,
            "dredgeignore": report.ignored_by_file,
        },
        "depth_truncated_dirs": report.depth_truncated,
        "pruned_dirs": report.pruned_dirs,
        "skipped_too_large": report.skipped_too_large,
    });
    let mut s = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| String::from("{\"schema_version\": 1}"));
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn shell_words_are_quoted_only_when_needed() {
        let report = Report {
            home: PathBuf::from("/h"),
            ..Report::default()
        };
        let w = |p: &str| shell_word(&report, Path::new(p));
        assert_eq!(w("/h/.config/nvim"), "~/.config/nvim");
        assert_eq!(w("/h/.config/my app"), "\"$HOME/.config/my app\"");
        assert_eq!(w("/h/a$b\"c"), "\"$HOME/a\\$b\\\"c\"");
        assert_eq!(w("/other/x y"), "\"/other/x y\"");
        assert_eq!(w("/other/x"), "/other/x");
        assert_eq!(w("/h"), "~/");
    }

    #[test]
    fn stow_package_names_are_plain() {
        assert_eq!(
            stow_package(Path::new(".config/nvim"), Some("neovim")),
            "neovim"
        );
        assert_eq!(stow_package(Path::new(".config/my app"), None), "my-app");
        assert_eq!(stow_package(Path::new(".tmux.conf"), None), "tmux.conf");
    }

    use super::*;

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
