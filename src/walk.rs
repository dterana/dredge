//! Safe bounded walker. Read-only: never follows symlinks, never opens
//! anything that is not a regular file or a directory.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Maximum depth inside a unit. A file directly in a unit dir is depth 1.
pub const MAX_DEPTH: usize = 6;

#[derive(Debug, Default)]
pub struct WalkResult {
    pub units: Vec<UnitFiles>,
    pub unreadable: Vec<PathBuf>,
    pub symlinks: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct UnitFiles {
    pub path: PathBuf,
    /// Regular files only.
    pub files: Vec<PathBuf>,
}

/// Spec roots. The first entry is `home` itself; `scan` recognises a root that
/// contains other roots as the home root and only yields its top level dot
/// entries, excluding children that are themselves scan roots (e.g. `.config`).
pub fn default_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.to_path_buf(),
        home.join(".config"),
        home.join("Library").join("Application Support"),
        home.join("Library").join("Preferences"),
    ]
}

/// Scan the default roots of `home`.
pub fn scan_home(home: &Path, allowed_link_targets: &[PathBuf]) -> WalkResult {
    scan(&default_roots(home), allowed_link_targets)
}

enum Resolved {
    Dir(PathBuf),
    Symlink(PathBuf),
    Skip,
    Unreadable,
}

/// Resolve a root without following a symlink at the root itself. An umbrella
/// (home) root is canonicalized fully, since `$HOME` may be a symlink.
fn resolve_root(root: &Path, umbrella: bool) -> Resolved {
    let canon = if umbrella {
        fs::canonicalize(root)
    } else {
        match (root.parent(), root.file_name()) {
            (Some(parent), Some(name)) => fs::canonicalize(parent).map(|p| p.join(name)),
            _ => fs::canonicalize(root),
        }
    };
    let canon = match canon {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Resolved::Skip,
        Err(_) => return Resolved::Unreadable,
    };
    match fs::symlink_metadata(&canon) {
        Ok(m) if m.file_type().is_symlink() => Resolved::Symlink(canon),
        Ok(m) if m.file_type().is_dir() => Resolved::Dir(canon),
        Ok(_) => Resolved::Skip,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Resolved::Skip,
        Err(_) => Resolved::Unreadable,
    }
}

/// Every depth-1 child of each root is a unit, except that a root containing
/// other roots is the home root: only its dot entries that are not themselves
/// roots are units.
pub fn scan(roots: &[PathBuf], _allowed_link_targets: &[PathBuf]) -> WalkResult {
    // Symlinks are listed and never followed regardless of target, so the
    // allowed targets need no special handling here; tracking uses them later.
    let mut result = WalkResult::default();
    let umbrella: Vec<bool> = roots
        .iter()
        .map(|r| roots.iter().any(|o| o != r && o.starts_with(r)))
        .collect();
    let resolved: Vec<Resolved> = roots
        .iter()
        .zip(&umbrella)
        .map(|(r, u)| resolve_root(r, *u))
        .collect();
    let root_dirs: Vec<PathBuf> = resolved
        .iter()
        .filter_map(|r| match r {
            Resolved::Dir(p) | Resolved::Symlink(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    for ((root, res), is_umbrella) in roots.iter().zip(resolved).zip(umbrella) {
        match res {
            Resolved::Dir(canon) => scan_root(&canon, is_umbrella, &root_dirs, &mut result),
            Resolved::Symlink(p) => result.symlinks.push(p),
            Resolved::Skip => {}
            Resolved::Unreadable => result.unreadable.push(root.clone()),
        }
    }
    result.units.sort_by(|a, b| a.path.cmp(&b.path));
    result.unreadable.sort();
    result.symlinks.sort();
    result
}

fn scan_root(root: &Path, dot_only: bool, other_roots: &[PathBuf], result: &mut WalkResult) {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => {
            result.unreadable.push(root.to_path_buf());
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                result.unreadable.push(root.to_path_buf());
                continue;
            }
        };
        if dot_only && !is_dot(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        if other_roots.contains(&path) {
            continue;
        }
        let ft = match fs::symlink_metadata(&path) {
            Ok(m) => m.file_type(),
            Err(_) => {
                result.unreadable.push(path);
                continue;
            }
        };
        if ft.is_symlink() {
            result.symlinks.push(path);
        } else if ft.is_file() {
            result.units.push(UnitFiles {
                files: vec![path.clone()],
                path,
            });
        } else if ft.is_dir() {
            let mut files = Vec::new();
            walk_dir(&path, 1, &mut files, result);
            files.sort();
            result.units.push(UnitFiles { path, files });
        }
        // sockets, FIFOs and devices are skipped without being opened
    }
}

fn walk_dir(dir: &Path, depth: usize, files: &mut Vec<PathBuf>, result: &mut WalkResult) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            result.unreadable.push(dir.to_path_buf());
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                result.unreadable.push(dir.to_path_buf());
                continue;
            }
        };
        let path = entry.path();
        let ft = match fs::symlink_metadata(&path) {
            Ok(m) => m.file_type(),
            Err(_) => {
                result.unreadable.push(path);
                continue;
            }
        };
        if ft.is_symlink() {
            result.symlinks.push(path);
        } else if ft.is_file() {
            files.push(path);
        } else if ft.is_dir() && depth < MAX_DEPTH {
            walk_dir(&path, depth + 1, files, result);
        }
    }
}

fn is_dot(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// Lossy conversion for display only; never use the result as a path.
pub fn display_lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
