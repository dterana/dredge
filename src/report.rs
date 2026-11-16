//! Human and JSON rendering. Neither renderer has access to secret values:
//! findings carry only rule id, path and line.
//!
//! Empty-section rule (human output): every core section header is always
//! printed, followed by `  (none)` when it has no entries. Sections that only
//! exist with `--all` (TRACKED, CACHE/STATE) follow the same rule when shown.
//! The closing "in repo, missing on this machine" note is a note, not a
//! section, and is printed only when it has entries.

use std::fmt::Write as _;
use std::path::Path;

use owo_colors::OwoColorize;
use serde_json::{json, Value};

use crate::model::{Finding, Kind, Report, Tracking, Unit};

/// Unknown units shown without `--all`.
pub const UNKNOWN_CAP: usize = 20;

const APP_W: usize = 14;
const PATH_W: usize = 44;

/// Display a path: `~/...` under home, `<repo dir name>/...` under the
/// dotfiles dir, else absolute. Lossy, for display only.
pub fn display_path(report: &Report, p: &Path) -> String {
    if !report.home.as_os_str().is_empty() {
        if let Ok(rel) = p.strip_prefix(&report.home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.to_string_lossy());
        }
    }
    if let Some(d) = &report.dotfiles {
        if let Ok(rel) = p.strip_prefix(d) {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.to_string_lossy().into_owned());
            if rel.as_os_str().is_empty() {
                return name;
            }
            return format!("{name}/{}", rel.to_string_lossy());
        }
    }
    p.to_string_lossy().into_owned()
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 1204 -> "1,204".
fn thousands(n: usize) -> String {
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
    let lines: Vec<String> = report
        .committed_findings
        .iter()
        .map(|f| {
            format!(
                "  {:<w$} {}",
                display_path(report, &f.path),
                p.paint(Tone::Danger, &format!("SECRET  {}", finding_text(f))),
                w = APP_W + 1 + PATH_W
            )
        })
        .collect();
    out.section(&h("COMMITTED SECRETS", !lines.is_empty()), &lines);

    // UNTRACKED SECRETS: every finding on this machine. Findings inside a
    // tracked unit or a `.dredgeignore`d unit are tagged, never hidden.
    let mut lines = Vec::new();
    for u in &report.units {
        for f in &u.findings {
            let mut line = format!(
                "  {} {:<PATH_W$} {}",
                app_col(u),
                display_path(report, &f.path),
                p.paint(Tone::Danger, &format!("SECRET  {}", finding_text(f)))
            );
            if u.ignored {
                line.push_str(&p.paint(Tone::Dim, "  (ignored by .dredgeignore)"));
            } else if is_tracked(u.tracking) {
                line.push_str(&p.paint(Tone::Dim, "  (in a tracked unit)"));
            }
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

    // UNKNOWN
    let unknown: Vec<&Unit> = report
        .units
        .iter()
        .filter(|u| u.kind == Kind::Unknown && is_untracked(u.tracking) && !u.ignored)
        .collect();
    let shown = if all {
        unknown.len()
    } else {
        unknown.len().min(UNKNOWN_CAP)
    };
    let mut header = format!("UNKNOWN ({})", thousands(unknown.len()));
    if shown < unknown.len() {
        let _ = write!(header, "  showing {shown}, run with --all to list");
    }
    let lines: Vec<String> = unknown[..shown]
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
    out.section(&h(&header, false), &lines);

    // COULD NOT READ
    let lines: Vec<String> = report
        .unreadable
        .iter()
        .map(|u| format!("  {}", display_path(report, u)))
        .collect();
    out.section(&h("COULD NOT READ", false), &lines);

    // IGNORED summary
    out.buf.push('\n');
    let _ = writeln!(
        out.buf,
        "{}  {} cache/state items, {} ignored by .dredgeignore",
        h("IGNORED", false),
        thousands(report.cache_state_count),
        thousands(report.ignored_by_file)
    );

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
        let lines: Vec<String> = report
            .missing_on_machine
            .iter()
            .map(|rel| format!("  {}", display_path(report, &report.home.join(rel))))
            .collect();
        out.section(&h("IN REPO, MISSING ON THIS MACHINE", false), &lines);
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
        "dotfiles": report.dotfiles.as_deref().map(lossy),
        "committed_secrets": report.committed_findings.iter().map(finding_json).collect::<Vec<_>>(),
        "units": units,
        "unreadable": report.unreadable.iter().map(|p| lossy(p)).collect::<Vec<_>>(),
        "missing_on_machine": report.missing_on_machine.iter().map(|p| lossy(p)).collect::<Vec<_>>(),
        "ignored": {
            "cache_state_items": report.cache_state_count,
            "dredgeignore": report.ignored_by_file,
        },
    });
    let mut s = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| String::from("{\"schema_version\": 1}"));
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
