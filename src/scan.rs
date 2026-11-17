//! Orchestration: resolve inputs, walk, classify, track, secret-scan, and
//! assemble the report. Read-only throughout.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;

use crate::classify::classify;
use crate::cli::{LayoutArg, ScanArgs};
use crate::dredgeignore::IgnoreList;
use crate::layout::{self, Index};
use crate::model::{Drift, Finding, Kind, Report, Tracking, Unit};
use crate::rules::Rules;
use crate::secrets::{scan_file, SecretRules};
use crate::track;
use crate::walk::{self, display_lossy, Root, SkippedRoot};

/// Depth cap for the committed-secrets walk of the dotfiles working tree.
pub const DOTFILES_MAX_DEPTH: usize = 16;

/// chezmoi's default source directory, relative to home. The only location
/// ever auto-detected.
const CHEZMOI_DEFAULT: &str = ".local/share/chezmoi";

/// Plists here are listed by name only, never read.
const PREFERENCES: &str = "Library/Preferences";

struct Ctx<'a> {
    home: &'a Path,
    dotfiles: Option<&'a Path>,
    index: &'a Index,
    rules: Rules,
    secrets: SecretRules,
}

/// One walked entry (regular file or symlink) after classification.
struct Entry {
    app: Option<String>,
    kind: Kind,
    /// Only for config and unknown entries.
    tracking: Option<Tracking>,
    drift: Option<Drift>,
    findings: Vec<Finding>,
}

pub fn run(args: &ScanArgs) -> Result<Report> {
    let home = resolve_home(args.home.as_deref())?;
    let (dotfiles, layout_arg) = resolve_dotfiles(args, &home)?;

    let (index, ignore) = match &dotfiles {
        Some(d) => {
            let layout = layout::detect(d, layout_arg);
            let index = Index::build(d, layout)
                .with_context(|| format!("cannot read dotfiles directory {}", display_lossy(d)))?;
            (index, IgnoreList::load(d))
        }
        None => (Index::default(), IgnoreList::default()),
    };

    let mut roots = walk::default_roots(&home);
    for r in &args.root {
        roots.push(Root::extra(
            std::path::absolute(r).unwrap_or_else(|_| r.clone()),
        ));
    }
    let excluded: Vec<PathBuf> = dotfiles.iter().filter(|d| **d != home).cloned().collect();
    let mut walked = walk::scan(&roots, &excluded);
    let notes: Vec<String> = walked.skipped.iter().map(skip_note).collect();

    // The dotfiles dir is compared against, never reported as home content.
    if let Some(d) = dotfiles.as_deref().filter(|d| *d != home.as_path()) {
        walked.units.retain(|u| !u.path.starts_with(d));
        for u in &mut walked.units {
            u.files.retain(|f| !f.starts_with(d));
        }
        walked.symlinks.retain(|s| !s.starts_with(d));
    }

    let ctx = Ctx {
        home: &home,
        dotfiles: dotfiles.as_deref(),
        index: &index,
        rules: Rules::embedded(),
        secrets: SecretRules::embedded(),
    };
    let mut unreadable = walked.unreadable;
    unreadable.extend(index.unreadable().iter().cloned());

    // Attribute nested symlinks to their walked unit; the rest are unit roots.
    let mut nested: Vec<Vec<PathBuf>> = vec![Vec::new(); walked.units.len()];
    let mut root_links = Vec::new();
    for link in walked.symlinks {
        match walked
            .units
            .iter()
            .position(|u| link != u.path && link.starts_with(&u.path))
        {
            Some(i) => nested[i].push(link),
            None => root_links.push(link),
        }
    }

    let mut units = Vec::new();
    let mut cache_state_count = 0;
    for (uf, links) in walked.units.into_iter().zip(nested) {
        let mut entries: Vec<Entry> = Vec::new();
        for f in &uf.files {
            entries.push(file_entry(&ctx, f, &mut unreadable));
        }
        for l in &links {
            entries.push(link_entry(&ctx, l, false));
        }
        let (unit, count) = assemble(&ctx, uf.path, false, entries, &ignore);
        cache_state_count += count;
        units.push(unit);
    }
    for link in root_links {
        let entry = link_entry(&ctx, &link, true);
        let (unit, count) = assemble(&ctx, link, true, vec![entry], &ignore);
        cache_state_count += count;
        units.push(unit);
    }
    units.sort_by(|a, b| a.path.cmp(&b.path));

    let mut committed_findings = match &dotfiles {
        Some(d) => scan_committed(d, &ctx.secrets, &mut unreadable),
        None => Vec::new(),
    };
    committed_findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));

    let missing_on_machine = track::missing_on_machine(&index, &home);

    unreadable.sort();
    unreadable.dedup();

    let mut report = Report {
        home: home.clone(),
        dotfiles: dotfiles.clone(),
        units,
        unreadable,
        ignored_by_file: 0,
        cache_state_count,
        committed_findings,
        missing_on_machine,
        notes,
    };
    ignore.apply(&mut report, &home);
    Ok(report)
}

/// The stderr note for an extra root that was not walked.
fn skip_note(s: &SkippedRoot) -> String {
    if s.dotfiles {
        format!(
            "skipping --root {}: inside the dotfiles directory {}, which is scanned only for committed secrets",
            display_lossy(&s.root),
            display_lossy(&s.covered_by)
        )
    } else {
        format!(
            "skipping --root {}: already covered by {}",
            display_lossy(&s.root),
            display_lossy(&s.covered_by)
        )
    }
}

fn resolve_home(arg: Option<&Path>) -> Result<PathBuf> {
    let raw = match arg {
        Some(h) => h.to_path_buf(),
        None => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .context("cannot determine the home directory: set HOME or pass --home")?,
    };
    match fs::canonicalize(&raw) {
        Ok(p) if p.is_dir() => Ok(p),
        _ => bail!(
            "home directory {} is not an existing directory",
            display_lossy(&raw)
        ),
    }
}

/// `--dotfiles` must be an existing directory. Without it, only chezmoi's
/// default source dir is auto-detected (and read with the chezmoi layout when
/// the layout is `auto`); otherwise there is no tracking info.
fn resolve_dotfiles(args: &ScanArgs, home: &Path) -> Result<(Option<PathBuf>, LayoutArg)> {
    if let Some(d) = &args.dotfiles {
        let is_dir = fs::metadata(d).map(|m| m.is_dir()).unwrap_or(false);
        if !is_dir {
            bail!(
                "--dotfiles {} is not an existing directory",
                display_lossy(d)
            );
        }
        let canon = fs::canonicalize(d)
            .with_context(|| format!("--dotfiles {} cannot be resolved", display_lossy(d)))?;
        return Ok((Some(canon), args.layout));
    }
    let chezmoi = home.join(CHEZMOI_DEFAULT);
    let is_dir = fs::metadata(&chezmoi).map(|m| m.is_dir()).unwrap_or(false);
    match fs::canonicalize(&chezmoi) {
        Ok(canon) if is_dir => {
            let layout = match args.layout {
                LayoutArg::Auto => LayoutArg::Chezmoi,
                other => other,
            };
            Ok((Some(canon), layout))
        }
        _ => Ok((None, args.layout)),
    }
}

fn home_rel(home: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(home)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| p.to_path_buf())
}

fn tracks(kind: Kind) -> bool {
    matches!(kind, Kind::Config | Kind::Unknown)
}

fn decide(ctx: &Ctx, path: &Path, rel: &Path) -> Tracking {
    match ctx.dotfiles {
        Some(d) => track::decide(path, rel, d, ctx.index),
        None => Tracking::Untracked,
    }
}

/// A regular file: classify, decide tracking and drift, scan for secrets.
fn file_entry(ctx: &Ctx, path: &Path, unreadable: &mut Vec<PathBuf>) -> Entry {
    let rel = home_rel(ctx.home, path);
    let (app, kind) = classify(&rel, &ctx.rules);
    let mut entry = Entry {
        app,
        kind,
        tracking: None,
        drift: None,
        findings: Vec::new(),
    };
    if !tracks(kind) {
        return entry;
    }
    let tracking = decide(ctx, path, &rel);
    if tracking == Tracking::TrackedDrifted {
        if let Some(repo) = ctx.index.lookup(&rel) {
            entry.drift = Some(Drift {
                path: path.to_path_buf(),
                repo_path: repo.repo_path,
            });
        }
    }
    entry.tracking = Some(tracking);
    let in_preferences = path.starts_with(ctx.home) && rel.starts_with(PREFERENCES);
    if !in_preferences {
        match scan_file(path, &ctx.secrets) {
            Ok(found) => entry.findings = found,
            Err(_) => unreadable.push(path.to_path_buf()),
        }
    }
    entry
}

/// A symlink is never read or followed; it is tracked only if it resolves
/// into the dotfiles dir.
fn link_entry(ctx: &Ctx, path: &Path, unit_root: bool) -> Entry {
    let rel = home_rel(ctx.home, path);
    let (mut app, mut kind) = classify(&rel, &ctx.rules);
    if unit_root && app.is_none() && kind == Kind::Unknown {
        // A symlinked directory: classify as if it were a file inside it.
        let (a, k) = classify(&rel.join("_"), &ctx.rules);
        app = a;
        kind = k;
    }
    let tracking = tracks(kind).then(|| decide(ctx, path, &rel));
    Entry {
        app,
        kind,
        tracking,
        drift: None,
        findings: Vec::new(),
    }
}

/// Builds the unit and returns how many of its entries count toward the
/// cache/state summary (all of them for a cache or state unit not ignored by
/// `.dredgeignore`, else none).
fn assemble(
    ctx: &Ctx,
    path: PathBuf,
    symlink: bool,
    entries: Vec<Entry>,
    ignore: &IgnoreList,
) -> (Unit, usize) {
    let has = |k: Kind| entries.iter().any(|e| e.kind == k);
    let kind = if has(Kind::Config) {
        Kind::Config
    } else if has(Kind::Unknown) {
        Kind::Unknown
    } else if has(Kind::State) {
        Kind::State
    } else {
        Kind::Cache
    };
    let app = entries.iter().find_map(|e| e.app.clone());
    let states: Vec<Tracking> = entries.iter().filter_map(|e| e.tracking).collect();
    let tracking = track::unit_tracking(&states);
    let count = if matches!(kind, Kind::Cache | Kind::State)
        && !ignore.is_ignored(&home_rel(ctx.home, &path))
    {
        entries.len()
    } else {
        0
    };
    let mut findings = Vec::new();
    let mut drifted = Vec::new();
    for e in entries {
        findings.extend(e.findings);
        drifted.extend(e.drift);
    }
    findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    let unit = Unit {
        path,
        app,
        kind,
        tracking,
        findings,
        ignored: false,
        symlink,
        drifted,
    };
    (unit, count)
}

/// Scans the dotfiles working tree, honoring only that repo's `.gitignore`.
/// Regular files only; symlinks are not followed; `.git` is skipped.
fn scan_committed(dir: &Path, rules: &SecretRules, unreadable: &mut Vec<PathBuf>) -> Vec<Finding> {
    let walker = WalkBuilder::new(dir)
        .standard_filters(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .hidden(false)
        .ignore(false)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(DOTFILES_MAX_DEPTH))
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut findings = Vec::new();
    for item in walker {
        let entry = match item {
            Ok(e) => e,
            Err(err) => {
                if let Some(p) = io_error_path(&err) {
                    unreadable.push(p);
                }
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        match scan_file(entry.path(), rules) {
            Ok(found) => findings.extend(found.into_iter().map(|mut f| {
                f.committed = true;
                f
            })),
            Err(_) => unreadable.push(entry.path().to_path_buf()),
        }
    }
    findings
}

/// The path of an I/O failure inside an `ignore` walk error, if any.
fn io_error_path(err: &ignore::Error) -> Option<PathBuf> {
    fn is_io(err: &ignore::Error) -> bool {
        match err {
            ignore::Error::Io(_) => true,
            ignore::Error::WithPath { err, .. }
            | ignore::Error::WithDepth { err, .. }
            | ignore::Error::WithLineNumber { err, .. } => is_io(err),
            _ => false,
        }
    }
    match err {
        ignore::Error::WithPath { path, err } if is_io(err) => Some(path.clone()),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            io_error_path(err)
        }
        ignore::Error::Partial(errs) => errs.iter().find_map(io_error_path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_path_finds_nested_path() {
        let err = ignore::Error::WithDepth {
            depth: 1,
            err: Box::new(ignore::Error::WithPath {
                path: PathBuf::from("/x/y"),
                err: Box::new(ignore::Error::Io(std::io::Error::other("denied"))),
            }),
        };
        assert_eq!(io_error_path(&err), Some(PathBuf::from("/x/y")));
        let glob = ignore::Error::WithPath {
            path: PathBuf::from("/x/.gitignore"),
            err: Box::new(ignore::Error::Glob {
                glob: None,
                err: "bad".into(),
            }),
        };
        assert_eq!(io_error_path(&glob), None);
    }
}
