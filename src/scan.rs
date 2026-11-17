//! Orchestration: resolve inputs, walk, classify, track, secret-scan, and
//! assemble the report. Read-only throughout.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use ignore::WalkBuilder;

use crate::classify::{classify, prunes_dir};
use crate::cli::{LayoutArg, ScanArgs};
use crate::dredgeignore::IgnoreList;
use crate::layout::{self, Index};
use crate::model::{Drift, Finding, Kind, Report, Tracking, Unit};
use crate::progress::Progress;
use crate::rules::Rules;
use crate::secrets::{scan_file, scan_file_detailed, SecretRules, SkipReason};
use crate::track;
use crate::walk::{self, display_lossy, Root, SkippedRoot, UnitFiles, WalkOptions};

/// Depth cap for the committed-secrets walk of the dotfiles working tree.
pub const DOTFILES_MAX_DEPTH: usize = 16;

/// chezmoi's default source directory, relative to home. The only location
/// ever auto-detected.
const CHEZMOI_DEFAULT: &str = ".local/share/chezmoi";

/// Plists here are listed by name only, never read.
const PREFERENCES: &str = "Library/Preferences";

/// Upper bound on the default number of unit-scanning workers.
const MAX_DEFAULT_JOBS: usize = 8;

/// How often the main thread refreshes the progress line while workers run.
const PROGRESS_POLL: Duration = Duration::from_millis(100);

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

/// Runs the scan without any progress output.
pub fn run(args: &ScanArgs) -> Result<Report> {
    run_with_progress(args, &mut Progress::disabled())
}

/// Runs the scan, reporting the number of files processed to `progress`. The
/// progress line is cleared before this returns, on success and on error.
pub fn run_with_progress(args: &ScanArgs, progress: &mut Progress) -> Result<Report> {
    let result = run_inner(args, progress);
    progress.finish();
    result
}

fn run_inner(args: &ScanArgs, progress: &mut Progress) -> Result<Report> {
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
    let rules = Rules::embedded();
    // Cache and state directories are not entered: the same test that
    // classifies their files, so pruning and classification agree.
    let prune = |dir: &Path| prunes_dir(&home_rel(&home, dir), &rules);
    let opts = WalkOptions {
        prune: Some(&prune),
    };
    let mut walked = walk::scan_with(&roots, &excluded, &opts);
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
        rules,
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
    let mut skipped_too_large = 0;
    let files_scanned = AtomicUsize::new(0);
    let work: Vec<(UnitFiles, Vec<PathBuf>)> = walked.units.into_iter().zip(nested).collect();
    let jobs = args.jobs.map_or_else(default_jobs, |n| n.get());
    let outcomes = par_map(&work, jobs, progress, &files_scanned, |(uf, links)| {
        scan_unit(&ctx, uf, links, &ignore, &files_scanned)
    })?;
    // Merged in walk order, so the result never depends on the worker count.
    for o in outcomes {
        units.push(o.unit);
        cache_state_count += o.cache_state;
        skipped_too_large += o.too_large;
        unreadable.extend(o.unreadable);
    }
    let mut files_done = files_scanned.load(Ordering::Relaxed);
    for link in root_links {
        let entry = link_entry(&ctx, &link, true);
        let (unit, count) = assemble(&ctx, link, true, vec![entry], &ignore);
        cache_state_count += count;
        units.push(unit);
    }
    units.sort_by(|a, b| a.path.cmp(&b.path));

    let mut committed_findings = match &dotfiles {
        Some(d) => scan_committed(d, &ctx.secrets, &mut unreadable, progress, &mut files_done),
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
        depth_truncated: walked.depth_truncated,
        pruned_dirs: walked.pruned_dirs,
        skipped_too_large,
        committed_findings,
        missing_on_machine,
        notes,
    };
    ignore.apply(&mut report, &home);
    Ok(report)
}

/// Workers used when `--jobs` is not given: one per CPU, at most
/// `MAX_DEFAULT_JOBS`, at least one.
fn default_jobs() -> usize {
    thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, MAX_DEFAULT_JOBS)
}

/// Applies `f` to every item on up to `jobs` scoped worker threads and returns
/// the results in item order, whatever the worker count. Workers claim items
/// through an atomic index and keep their results local; nothing is locked.
/// Meanwhile this thread shows `counter` on `progress`. A worker that panics
/// turns into an error that carries no item content.
fn par_map<T, R, F>(
    items: &[T],
    jobs: usize,
    progress: &mut Progress,
    counter: &AtomicUsize,
    f: F,
) -> Result<Vec<R>>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = jobs.min(items.len()).max(1);
    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<R>> = items.iter().map(|_| None).collect();
    let main = thread::current();
    thread::scope(|s| -> Result<()> {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let main = main.clone();
                let (next, f) = (&next, &f);
                s.spawn(move || {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else {
                            break;
                        };
                        done.push((i, f(item)));
                    }
                    main.unpark();
                    done
                })
            })
            .collect();
        while !handles.iter().all(|h| h.is_finished()) {
            progress.tick(counter.load(Ordering::Relaxed));
            thread::park_timeout(PROGRESS_POLL);
        }
        progress.tick(counter.load(Ordering::Relaxed));
        for h in handles {
            let done = h
                .join()
                .map_err(|_| anyhow!("internal error: a scan worker stopped unexpectedly"))?;
            for (i, r) in done {
                if let Some(slot) = slots.get_mut(i) {
                    *slot = Some(r);
                }
            }
        }
        Ok(())
    })?;
    slots
        .into_iter()
        .collect::<Option<Vec<R>>>()
        .context("internal error: a unit was not scanned")
}

/// What scanning one unit produced.
struct UnitOutcome {
    unit: Unit,
    /// Entries counted toward the cache/state summary.
    cache_state: usize,
    unreadable: Vec<PathBuf>,
    too_large: usize,
}

/// Classifies, tracks and secret-scans one walked unit. Runs on a worker:
/// its only shared write is one relaxed increment of `files_scanned` per file.
fn scan_unit(
    ctx: &Ctx,
    uf: &UnitFiles,
    links: &[PathBuf],
    ignore: &IgnoreList,
    files_scanned: &AtomicUsize,
) -> UnitOutcome {
    let mut unreadable = Vec::new();
    let mut too_large = 0;
    if uf.pruned {
        return UnitOutcome {
            unit: pruned_unit(ctx, uf.path.clone()),
            cache_state: 0,
            unreadable,
            too_large,
        };
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(uf.files.len() + links.len());
    for f in &uf.files {
        entries.push(file_entry(ctx, f, &mut unreadable, &mut too_large));
        files_scanned.fetch_add(1, Ordering::Relaxed);
    }
    for l in links {
        entries.push(link_entry(ctx, l, false));
    }
    let (unit, cache_state) = assemble(ctx, uf.path.clone(), false, entries, ignore);
    UnitOutcome {
        unit,
        cache_state,
        unreadable,
        too_large,
    }
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
fn file_entry(
    ctx: &Ctx,
    path: &Path,
    unreadable: &mut Vec<PathBuf>,
    too_large: &mut usize,
) -> Entry {
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
        match scan_file_detailed(path, &ctx.secrets) {
            Ok(outcome) => {
                if outcome.skipped == Some(SkipReason::TooLarge) {
                    *too_large += 1;
                }
                entry.findings = outcome.findings;
            }
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

/// A unit whose root directory was not entered: it lists no files, so it
/// takes the app and kind its own directory classifies as (cache or state,
/// since that is what pruned it) and counts nothing toward the summary.
fn pruned_unit(ctx: &Ctx, path: PathBuf) -> Unit {
    let (app, kind) = classify(&home_rel(ctx.home, &path).join("_"), &ctx.rules);
    Unit {
        path,
        app,
        kind,
        tracking: Tracking::Untracked,
        findings: Vec::new(),
        ignored: false,
        symlink: false,
        drifted: Vec::new(),
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
    // The label comes from an entry that decided the unit's kind. Only a
    // state or cache unit may borrow a label from its other entries.
    let app = entries
        .iter()
        .filter(|e| e.kind == kind)
        .find_map(|e| e.app.clone())
        .or_else(|| {
            matches!(kind, Kind::State | Kind::Cache)
                .then(|| entries.iter().find_map(|e| e.app.clone()))
                .flatten()
        });
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
fn scan_committed(
    dir: &Path,
    rules: &SecretRules,
    unreadable: &mut Vec<PathBuf>,
    progress: &mut Progress,
    files_done: &mut usize,
) -> Vec<Finding> {
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
        *files_done += 1;
        progress.tick(*files_done);
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

    fn map_with(jobs: usize, items: &[usize]) -> Vec<String> {
        let counter = AtomicUsize::new(0);
        par_map(items, jobs, &mut Progress::disabled(), &counter, |&i| {
            // Uneven work, so workers finish items out of order.
            if i % 7 == 0 {
                thread::sleep(Duration::from_millis(2));
            }
            counter.fetch_add(1, Ordering::Relaxed);
            format!("item-{i}")
        })
        .unwrap()
    }

    #[test]
    fn par_map_keeps_item_order_for_any_worker_count() {
        let items: Vec<usize> = (0..300).collect();
        let want: Vec<String> = items.iter().map(|i| format!("item-{i}")).collect();
        for jobs in [1, 2, 3, 8, 64, 1000] {
            assert_eq!(map_with(jobs, &items), want, "jobs {jobs}");
        }
        assert!(map_with(4, &[]).is_empty());
    }

    #[test]
    fn par_map_turns_a_worker_panic_into_an_error() {
        let items: Vec<usize> = (0..50).collect();
        for jobs in [1, 4] {
            let counter = AtomicUsize::new(0);
            let r = par_map(&items, jobs, &mut Progress::disabled(), &counter, |&i| {
                if i == 13 {
                    panic!("boom");
                }
                i
            });
            let msg = format!("{:#}", r.err().unwrap());
            assert_eq!(msg, "internal error: a scan worker stopped unexpectedly");
        }
    }

    #[test]
    fn default_jobs_is_between_one_and_eight() {
        assert!((1..=MAX_DEFAULT_JOBS).contains(&default_jobs()));
    }

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
