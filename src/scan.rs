//! Orchestration: resolve inputs, walk, classify, track, secret-scan, and
//! assemble the report. Read-only throughout.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use ignore::WalkBuilder;

use crate::classify::{classify_in_unit, prunes_dir};
use crate::cli::{LayoutArg, ScanArgs};
use crate::dredgeignore::IgnoreList;
use crate::layout::{self, Index};
use crate::model::{Drift, Finding, Kind, Report, Tracking, Unit};
use crate::progress::Progress;
use crate::rules::Rules;
use crate::secrets::{scan_file, scan_file_detailed, SecretRules, SkipReason};
use crate::track::{self, Source};
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

/// Upper bound on worker threads, whatever `--jobs` asks for.
const MAX_JOBS: usize = 64;

/// How often the main thread refreshes the progress line while workers run.
const PROGRESS_POLL: Duration = Duration::from_millis(100);

struct Ctx<'a> {
    home: &'a Path,
    /// Every dotfiles directory with its index, in the order given.
    sources: &'a [Source],
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

    // Each directory gets its own layout detection and its own index.
    let mut sources = Vec::with_capacity(dotfiles.len());
    for d in &dotfiles {
        let layout = layout::detect(d, layout_arg);
        let index = Index::build(d, layout)
            .with_context(|| format!("cannot read dotfiles directory {}", display_lossy(d)))?;
        sources.push(Source {
            dir: d.clone(),
            index,
        });
    }
    let ignore = match dotfiles.first() {
        Some(d) => IgnoreList::load(d),
        None => IgnoreList::default(),
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
    let visited = AtomicUsize::new(0);
    let opts = WalkOptions {
        prune: Some(&prune),
        visited: Some(&visited),
    };
    let mut walked = in_background(progress, &visited, Progress::walking, || {
        walk::scan_with(&roots, &excluded, &opts)
    })?;
    let notes: Vec<String> = walked.skipped.iter().map(skip_note).collect();

    // Dotfiles dirs are compared against, never reported as home content.
    let inside_dotfiles = |p: &Path| excluded.iter().any(|d| p.starts_with(d));
    if !excluded.is_empty() {
        walked.units.retain(|u| !inside_dotfiles(&u.path));
        for u in &mut walked.units {
            u.files.retain(|f| !inside_dotfiles(f));
        }
        walked.symlinks.retain(|s| !inside_dotfiles(s));
    }

    let mut unreadable = walked.unreadable;
    for s in &sources {
        unreadable.extend(s.index.unreadable().iter().cloned());
    }

    // Attribute nested symlinks to the unit whose walk found them: the deepest
    // unit that contains the link with no pruned directory in between (a link
    // under a `--root` inside a pruned directory is that root's own unit). The
    // rest are unit roots.
    let found_by = |u: &UnitFiles, link: &Path| {
        link != u.path
            && link.starts_with(&u.path)
            && !u.pruned
            && link
                .parent()
                .into_iter()
                .flat_map(Path::ancestors)
                .take_while(|a| *a != u.path)
                .all(|a| !prune(a))
    };
    let mut nested: Vec<Vec<PathBuf>> = vec![Vec::new(); walked.units.len()];
    let mut root_links = Vec::new();
    for link in walked.symlinks {
        let owner = walked
            .units
            .iter()
            .enumerate()
            .filter(|(_, u)| found_by(u, &link))
            .max_by_key(|(_, u)| u.path.components().count())
            .map(|(i, _)| i);
        match owner {
            Some(i) => nested[i].push(link),
            None => root_links.push(link),
        }
    }

    let ctx = Ctx {
        home: &home,
        sources: &sources,
        rules,
        secrets: SecretRules::embedded(),
    };

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
        let entry = link_entry(&ctx, &link, &link);
        let (unit, count) = assemble(&ctx, link, true, vec![entry], &ignore);
        cache_state_count += count;
        units.push(unit);
    }
    units.sort_by(|a, b| a.path.cmp(&b.path));

    let mut committed_findings = Vec::new();
    for d in &dotfiles {
        committed_findings.extend(scan_committed(
            d,
            &ctx.secrets,
            &mut unreadable,
            progress,
            &mut files_done,
        ));
    }
    committed_findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));

    let missing_on_machine = track::missing_on_machine_all(&sources, &home);

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

/// Runs `work` on a scoped thread while this thread shows `counter` through
/// `show` on `progress`. A panic in `work` becomes an error without content.
fn in_background<R: Send>(
    progress: &mut Progress,
    counter: &AtomicUsize,
    show: fn(&mut Progress, usize),
    work: impl FnOnce() -> R + Send,
) -> Result<R> {
    let main = thread::current();
    thread::scope(|s| {
        let handle = thread::Builder::new()
            .spawn_scoped(s, move || {
                let r = work();
                main.unpark();
                r
            })
            .map_err(|_| anyhow!("internal error: cannot start the walk thread"))?;
        poll(progress, counter, show, || handle.is_finished());
        handle
            .join()
            .map_err(|_| anyhow!("internal error: the walk stopped unexpectedly"))
    })
}

/// Replaces the default panic hook with one that prints nothing. A panic
/// message or location could carry content; every panic is instead reported
/// by the caller as a one-line internal error.
pub fn silence_panics() {
    std::panic::set_hook(Box::new(|_| {}));
}

/// Workers for `items` units when `jobs` are asked for: at most `MAX_JOBS`,
/// at most one per item, at least one.
fn worker_count(jobs: usize, items: usize) -> usize {
    jobs.min(MAX_JOBS).min(items).max(1)
}

/// Shows `counter` every `PROGRESS_POLL` (or sooner when unparked) until
/// `finished`, then once more with the final count.
fn poll(
    progress: &mut Progress,
    counter: &AtomicUsize,
    show: fn(&mut Progress, usize),
    finished: impl Fn() -> bool,
) {
    while !finished() {
        show(progress, counter.load(Ordering::Relaxed));
        thread::park_timeout(PROGRESS_POLL);
    }
    show(progress, counter.load(Ordering::Relaxed));
}

/// Applies `f` to every item on up to `jobs` scoped worker threads and returns
/// the results in item order, whatever the worker count. Workers claim items
/// through an atomic index and keep their results local; nothing is locked.
/// Meanwhile this thread shows `counter` on `progress`. Any worker panic
/// turns into one error that carries no item content. If some workers cannot
/// be started the others do all the work; if none can, that is an error.
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
    let workers = worker_count(jobs, items.len());
    let next = AtomicUsize::new(0);
    let mut slots: Vec<Option<R>> = items.iter().map(|_| None).collect();
    let main = thread::current();
    thread::scope(|s| -> Result<()> {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let main = main.clone();
            let (next, f) = (&next, &f);
            let spawned = thread::Builder::new().spawn_scoped(s, move || {
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
            });
            match spawned {
                Ok(h) => handles.push(h),
                Err(_) => break,
            }
        }
        if handles.is_empty() {
            bail!("internal error: cannot start a scan worker thread");
        }
        poll(progress, counter, Progress::tick, || {
            handles.iter().all(|h| h.is_finished())
        });
        // Join every worker before looking at any result: a scoped thread
        // left unjoined after a panic would make the scope itself panic.
        let joined: Vec<_> = handles.into_iter().map(|h| h.join()).collect();
        let mut failed = false;
        for j in joined {
            match j {
                Ok(done) => {
                    for (i, r) in done {
                        if let Some(slot) = slots.get_mut(i) {
                            *slot = Some(r);
                        }
                    }
                }
                Err(_) => failed = true,
            }
        }
        if failed {
            bail!("internal error: a scan worker stopped unexpectedly");
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
        entries.push(file_entry(
            ctx,
            &uf.path,
            f,
            &mut unreadable,
            &mut too_large,
        ));
        files_scanned.fetch_add(1, Ordering::Relaxed);
    }
    for l in links {
        entries.push(link_entry(ctx, &uf.path, l));
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

/// Every `--dotfiles` must be an existing directory; they are canonicalized
/// and deduplicated, keeping the first occurrence of each. Without any, only
/// chezmoi's default source dir is auto-detected (and read with the chezmoi
/// layout when the layout is `auto`); otherwise there is no tracking info.
fn resolve_dotfiles(args: &ScanArgs, home: &Path) -> Result<(Vec<PathBuf>, LayoutArg)> {
    if !args.dotfiles.is_empty() {
        let mut dirs: Vec<PathBuf> = Vec::with_capacity(args.dotfiles.len());
        for d in &args.dotfiles {
            let is_dir = fs::metadata(d).map(|m| m.is_dir()).unwrap_or(false);
            if !is_dir {
                bail!(
                    "--dotfiles {} is not an existing directory",
                    display_lossy(d)
                );
            }
            let canon = fs::canonicalize(d)
                .with_context(|| format!("--dotfiles {} cannot be resolved", display_lossy(d)))?;
            if !dirs.contains(&canon) {
                dirs.push(canon);
            }
        }
        return Ok((dirs, args.layout));
    }
    let chezmoi = home.join(CHEZMOI_DEFAULT);
    let is_dir = fs::metadata(&chezmoi).map(|m| m.is_dir()).unwrap_or(false);
    match fs::canonicalize(&chezmoi) {
        Ok(canon) if is_dir => {
            let layout = match args.layout {
                LayoutArg::Auto => LayoutArg::Chezmoi,
                other => other,
            };
            Ok((vec![canon], layout))
        }
        _ => Ok((Vec::new(), args.layout)),
    }
}

fn home_rel(home: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(home)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| p.to_path_buf())
}

/// `p` relative to the directory that holds `unit`, so it starts with the
/// unit's own name. Only these components may make an entry cache.
fn unit_rel<'a>(unit: &Path, p: &'a Path) -> &'a Path {
    unit.parent()
        .and_then(|d| p.strip_prefix(d).ok())
        .unwrap_or_else(|| Path::new(p.file_name().unwrap_or_default()))
}

fn tracks(kind: Kind) -> bool {
    matches!(kind, Kind::Config | Kind::Unknown)
}

fn decide(ctx: &Ctx, path: &Path, rel: &Path) -> Tracking {
    if ctx.sources.is_empty() {
        return Tracking::Untracked;
    }
    track::decide_all(path, rel, ctx.sources)
}

/// A regular file: classify, decide tracking and drift, scan for secrets.
fn file_entry(
    ctx: &Ctx,
    unit: &Path,
    path: &Path,
    unreadable: &mut Vec<PathBuf>,
    too_large: &mut usize,
) -> Entry {
    let rel = home_rel(ctx.home, path);
    let (app, kind) = classify_in_unit(&rel, unit_rel(unit, path), &ctx.rules);
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
        if let Some(repo) = track::lookup_first(ctx.sources, &rel) {
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
/// into a dotfiles dir.
fn link_entry(ctx: &Ctx, unit: &Path, path: &Path) -> Entry {
    let rel = home_rel(ctx.home, path);
    let in_unit = unit_rel(unit, path);
    let (mut app, mut kind) = classify_in_unit(&rel, in_unit, &ctx.rules);
    if unit == path && app.is_none() && kind == Kind::Unknown {
        // A symlinked directory: classify as if it were a file inside it.
        let (a, k) = classify_in_unit(&rel.join("_"), &in_unit.join("_"), &ctx.rules);
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
    let probe = home_rel(ctx.home, &path).join("_");
    let (app, kind) = classify_in_unit(&probe, &unit_rel(&path, &path).join("_"), &ctx.rules);
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
    fn panics_in_several_workers_are_one_clean_error() {
        let items: Vec<usize> = (0..400).collect();
        for jobs in [2, 4, 8] {
            let counter = AtomicUsize::new(0);
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                par_map(&items, jobs, &mut Progress::disabled(), &counter, |&i| {
                    if i % 50 == 0 {
                        thread::sleep(Duration::from_millis(5));
                        panic!("boom");
                    }
                    i
                })
            }));
            let r = caught.unwrap_or_else(|_| panic!("par_map panicked with jobs {jobs}"));
            let msg = format!("{:#}", r.err().unwrap());
            assert_eq!(msg, "internal error: a scan worker stopped unexpectedly");
        }
    }

    #[test]
    fn worker_count_is_clamped() {
        assert_eq!(worker_count(100_000, 1_000), MAX_JOBS);
        assert_eq!(worker_count(100_000, 3), 3);
        assert_eq!(worker_count(4, 0), 1);
        assert_eq!(worker_count(1, 500), 1);
        assert_eq!(MAX_JOBS, 64);
        // Observed: no more than MAX_JOBS distinct threads do the work.
        let items: Vec<usize> = (0..1_000).collect();
        let seen = std::sync::Mutex::new(std::collections::HashSet::new());
        let counter = AtomicUsize::new(0);
        par_map(&items, 100_000, &mut Progress::disabled(), &counter, |_| {
            thread::sleep(Duration::from_millis(1));
            if let Ok(mut s) = seen.lock() {
                s.insert(thread::current().id());
            }
        })
        .unwrap();
        let n = seen.lock().unwrap().len();
        assert!((1..=MAX_JOBS).contains(&n), "{n} threads");
    }

    /// Run only as a child of `worker_panic_prints_nothing_of_its_own`.
    #[test]
    #[ignore]
    fn quiet_panic_child() {
        if std::env::var_os("DREDGE_QUIET_PANIC_CHILD").is_none() {
            return;
        }
        crate::scan::silence_panics();
        let items: Vec<usize> = (0..100).collect();
        let counter = AtomicUsize::new(0);
        let r = par_map(&items, 4, &mut Progress::disabled(), &counter, |&i| {
            if i % 10 == 0 {
                panic!("boom-payload at item {i}");
            }
            i
        });
        println!(
            "RESULT: {:#}",
            r.err().map(|e| format!("{e:#}")).unwrap_or_default()
        );
    }

    #[test]
    fn worker_panic_prints_nothing_of_its_own() {
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "scan::tests::quiet_panic_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("DREDGE_QUIET_PANIC_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stdout.contains("RESULT: internal error: a scan worker stopped unexpectedly"),
            "{stdout}"
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert!(
            !stderr.contains("boom") && !stdout.contains("boom"),
            "{stderr}"
        );
        assert!(!stderr.contains("scan.rs"), "{stderr}");
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
