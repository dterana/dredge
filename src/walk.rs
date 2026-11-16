#![allow(dead_code)]

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

/// Spec roots. The first entry is `home` itself, whose units are only its
/// top level dot entries (use `scan_home`, which applies that filter).
pub fn default_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.to_path_buf(),
        home.join(".config"),
        home.join("Library").join("Application Support"),
        home.join("Library").join("Preferences"),
    ]
}

/// Scan the default roots of `home`, restricting the `~` root to dot entries.
pub fn scan_home(home: &Path, allowed_link_targets: &[PathBuf]) -> WalkResult {
    scan_opts(
        &default_roots(home),
        allowed_link_targets,
        std::slice::from_ref(&home.to_path_buf()),
    )
}

/// Every depth-1 child of each root is a unit.
pub fn scan(roots: &[PathBuf], allowed_link_targets: &[PathBuf]) -> WalkResult {
    scan_opts(roots, allowed_link_targets, &[])
}

/// Like `scan`, but roots listed in `dot_only_roots` only yield units whose
/// name starts with a dot.
pub fn scan_opts(
    roots: &[PathBuf],
    // Symlinks are listed and never followed regardless of target, so the
    // allowed targets need no special handling here; tracking uses them later.
    _allowed_link_targets: &[PathBuf],
    dot_only_roots: &[PathBuf],
) -> WalkResult {
    let mut result = WalkResult::default();
    let dot_only: Vec<PathBuf> = dot_only_roots
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .collect();
    for root in roots {
        let canon = match fs::canonicalize(root) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                result.unreadable.push(root.clone());
                continue;
            }
        };
        let dots = dot_only.contains(&canon);
        scan_root(&canon, dots, &mut result);
    }
    result.units.sort_by(|a, b| a.path.cmp(&b.path));
    result.unreadable.sort();
    result.symlinks.sort();
    result
}

fn scan_root(root: &Path, dot_only: bool, result: &mut WalkResult) {
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
