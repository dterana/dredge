use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::layout::{Index, RepoEntry};
use crate::model::Tracking;
use crate::secrets::open_regular;

/// Streaming hash. Ok(None) when the path is not a regular file (symlink,
/// FIFO, ...) at open time; the open never follows symlinks or blocks.
fn sha256_file(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let Some(mut f) = open_regular(path)? else {
        return Ok(None);
    };
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Some(h.finalize().to_vec()))
}

/// One dotfiles directory and its index.
#[derive(Debug)]
pub struct Source {
    /// Canonical directory.
    pub dir: PathBuf,
    pub index: Index,
}

pub fn decide(home_file: &Path, home_rel: &Path, dotfiles: &Path, index: &Index) -> Tracking {
    let root = fs::canonicalize(dotfiles).unwrap_or_else(|_| dotfiles.to_path_buf());
    decide_with(
        home_file,
        home_rel,
        std::iter::once((root.as_path(), index)),
    )
}

/// `decide` against several dotfiles directories: a symlink that resolves
/// into any of them is tracked; a regular file is compared with the entry of
/// the first directory (in `sources` order) whose index defines it.
pub fn decide_all(home_file: &Path, home_rel: &Path, sources: &[Source]) -> Tracking {
    decide_with(
        home_file,
        home_rel,
        sources.iter().map(|s| (s.dir.as_path(), &s.index)),
    )
}

/// The repo entry for `home_rel` from the first source that defines it.
pub fn lookup_first(sources: &[Source], home_rel: &Path) -> Option<RepoEntry> {
    sources.iter().find_map(|s| s.index.lookup(home_rel))
}

/// `sources` yields each directory, already canonical, with its index.
fn decide_with<'a, I>(home_file: &Path, home_rel: &Path, sources: I) -> Tracking
where
    I: Iterator<Item = (&'a Path, &'a Index)> + Clone,
{
    let Ok(meta) = fs::symlink_metadata(home_file) else {
        return Tracking::Untracked;
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        // Resolve only; never open the target.
        let Ok(t) = fs::canonicalize(home_file) else {
            return Tracking::Untracked;
        };
        let inside = sources.map(|(d, _)| d).any(|d| t.starts_with(d));
        return if inside {
            Tracking::Tracked
        } else {
            Tracking::Untracked
        };
    }
    if !ft.is_file() {
        return Tracking::Untracked;
    }
    let Some(entry) = sources.clone().find_map(|(_, i)| i.lookup(home_rel)) else {
        return Tracking::Untracked;
    };
    if entry.unverified {
        return Tracking::TrackedUnverified;
    }
    if entry.template {
        return Tracking::Tracked;
    }
    // The repo side must itself be a regular file (not a symlink/FIFO).
    match fs::symlink_metadata(&entry.repo_path) {
        Ok(m) if m.file_type().is_file() => {}
        _ => return Tracking::TrackedUnverified,
    }
    // Home side first: not hashable (swapped to symlink/FIFO, unreadable)
    // means we cannot call it tracked.
    let Ok(Some(home_hash)) = sha256_file(home_file) else {
        return Tracking::Untracked;
    };
    match sha256_file(&entry.repo_path) {
        Ok(Some(repo_hash)) if repo_hash == home_hash => Tracking::Tracked,
        Ok(Some(_)) => Tracking::TrackedDrifted,
        _ => Tracking::TrackedUnverified,
    }
}

/// Aggregate per-file states of one unit. `TrackedDrifted` and
/// `TrackedUnverified` count as tracked for the all/some/none decision.
/// If every file is tracked: any drifted -> `TrackedDrifted`, else any
/// unverified -> `TrackedUnverified`, else `Tracked`. Some tracked and some
/// not -> `PartiallyTracked`; none tracked or empty -> `Untracked`.
pub fn unit_tracking(file_states: &[Tracking]) -> Tracking {
    let is_tracked = |t: &Tracking| {
        matches!(
            t,
            Tracking::Tracked | Tracking::TrackedDrifted | Tracking::TrackedUnverified
        )
    };
    let tracked = file_states.iter().filter(|t| is_tracked(t)).count();
    if tracked == 0 {
        Tracking::Untracked
    } else if tracked < file_states.len() {
        Tracking::PartiallyTracked
    } else if file_states.contains(&Tracking::TrackedDrifted) {
        Tracking::TrackedDrifted
    } else if file_states.contains(&Tracking::TrackedUnverified) {
        Tracking::TrackedUnverified
    } else {
        Tracking::Tracked
    }
}

/// Repo entries whose home path does not exist (dangling symlinks exist).
pub fn missing_on_machine(index: &Index, home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = index
        .entries()
        .filter(|(rel, _)| {
            matches!(fs::symlink_metadata(home.join(rel)), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        })
        .map(|(rel, _)| rel.clone())
        .collect();
    out.sort();
    out
}

/// `missing_on_machine` across every source: the union, sorted, each path once.
pub fn missing_on_machine_all(sources: &[Source], home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = sources
        .iter()
        .flat_map(|s| missing_on_machine(&s.index, home))
        .collect();
    out.sort();
    out.dedup();
    out
}
