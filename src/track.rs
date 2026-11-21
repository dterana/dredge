use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::layout::{Index, RepoEntry};
use crate::model::Tracking;
use crate::secrets::open_regular;

/// Largest file that is hashed. A drifted file above this is not read to the
/// end: differing sizes already prove drift, equal sizes stay unverified.
const MAX_HASH_BYTES: u64 = 128 * 1024 * 1024;

/// A file's digest, or its size when it is too large to hash.
#[derive(Debug, PartialEq, Eq)]
enum Hashed {
    Digest(Vec<u8>, u64),
    /// Over the cap; the size is a lower bound when the file grew during the read.
    TooLarge(u64),
}

impl Hashed {
    fn len(&self) -> u64 {
        match self {
            Hashed::Digest(_, n) | Hashed::TooLarge(n) => *n,
        }
    }
}

/// Streaming hash, read at most `cap` bytes. Ok(None) when the path is not a
/// regular file (symlink, FIFO, ...) at open time; the open never follows
/// symlinks or blocks.
fn sha256_file(path: &Path, cap: u64) -> std::io::Result<Option<Hashed>> {
    let Some(f) = open_regular(path)? else {
        return Ok(None);
    };
    let size = f.metadata()?.len();
    if size > cap {
        return Ok(Some(Hashed::TooLarge(size)));
    }
    let mut f = f.take(cap + 1);
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        h.update(&buf[..n]);
    }
    if total > cap {
        return Ok(Some(Hashed::TooLarge(total)));
    }
    Ok(Some(Hashed::Digest(h.finalize().to_vec(), total)))
}

/// Compare the two sides. Equal digests are tracked, different ones drifted.
/// When a side is too large to hash, differing sizes still prove drift and
/// equal sizes cannot be verified.
fn compare(home: &Hashed, repo: &Hashed) -> Tracking {
    match (home, repo) {
        (Hashed::Digest(a, _), Hashed::Digest(b, _)) if a == b => Tracking::Tracked,
        (Hashed::Digest(..), Hashed::Digest(..)) => Tracking::TrackedDrifted,
        _ if home.len() != repo.len() => Tracking::TrackedDrifted,
        _ => Tracking::TrackedUnverified,
    }
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
    let Ok(Some(home_hash)) = sha256_file(home_file, MAX_HASH_BYTES) else {
        return Tracking::Untracked;
    };
    match sha256_file(&entry.repo_path, MAX_HASH_BYTES) {
        Ok(Some(repo_hash)) => compare(&home_hash, &repo_hash),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dredge-track-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hashes_a_file_within_the_cap() {
        let d = tmp("within");
        let p = file(&d, "a", b"hello");
        let h = sha256_file(&p, 5).unwrap().unwrap();
        assert!(matches!(h, Hashed::Digest(_, 5)));
    }

    #[test]
    fn a_file_over_the_cap_is_not_read() {
        let d = tmp("over");
        let p = file(&d, "a", b"hello!");
        assert_eq!(sha256_file(&p, 5).unwrap().unwrap(), Hashed::TooLarge(6));
    }

    #[test]
    fn too_large_with_different_sizes_is_drifted() {
        assert_eq!(
            compare(&Hashed::TooLarge(10), &Hashed::TooLarge(11)),
            Tracking::TrackedDrifted
        );
        assert_eq!(
            compare(&Hashed::TooLarge(10), &Hashed::Digest(vec![1], 4)),
            Tracking::TrackedDrifted
        );
    }

    #[test]
    fn too_large_with_equal_sizes_is_unverified() {
        assert_eq!(
            compare(&Hashed::TooLarge(10), &Hashed::TooLarge(10)),
            Tracking::TrackedUnverified
        );
    }

    #[test]
    fn digests_decide_when_both_fit() {
        let a = Hashed::Digest(vec![1], 3);
        assert_eq!(compare(&a, &Hashed::Digest(vec![1], 3)), Tracking::Tracked);
        assert_eq!(
            compare(&a, &Hashed::Digest(vec![2], 3)),
            Tracking::TrackedDrifted
        );
    }
}
