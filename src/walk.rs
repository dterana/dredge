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
    /// Extra roots that were not walked because another root already covers them.
    pub skipped: Vec<SkippedRoot>,
}

#[derive(Debug)]
pub struct UnitFiles {
    pub path: PathBuf,
    /// Regular files only.
    pub files: Vec<PathBuf>,
}

/// How a root is walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    /// The home directory: only its top level dot entries are units. The only
    /// root that is ever dot-filtered.
    Home,
    /// A built-in root (`~/.config` and the like): every depth-1 child is a unit.
    Builtin,
    /// A `--root` entry: like `Builtin`, but skipped when another root already
    /// walks it.
    Extra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub kind: RootKind,
}

impl Root {
    pub fn home(path: PathBuf) -> Root {
        Root {
            path,
            kind: RootKind::Home,
        }
    }
    pub fn builtin(path: PathBuf) -> Root {
        Root {
            path,
            kind: RootKind::Builtin,
        }
    }
    pub fn extra(path: PathBuf) -> Root {
        Root {
            path,
            kind: RootKind::Extra,
        }
    }
}

/// An extra root left out because `covered_by` (a walked root, or an excluded
/// directory when `dotfiles` is set) already contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRoot {
    pub root: PathBuf,
    pub covered_by: PathBuf,
    pub dotfiles: bool,
}

/// Spec roots: `home` itself (dot entries only), `~/.config`,
/// `~/Library/Application Support` and `~/Library/Preferences`.
pub fn default_roots(home: &Path) -> Vec<Root> {
    vec![
        Root::home(home.to_path_buf()),
        Root::builtin(home.join(".config")),
        Root::builtin(home.join("Library").join("Application Support")),
        Root::builtin(home.join("Library").join("Preferences")),
    ]
}

/// Scan the default roots of `home`.
pub fn scan_home(home: &Path) -> WalkResult {
    scan(&default_roots(home), &[])
}

enum Resolved {
    Dir(PathBuf),
    Symlink(PathBuf),
    Skip,
    Unreadable,
}

/// Resolve a root without following a symlink at the root itself. The home
/// root is canonicalized fully, since `$HOME` may be a symlink.
fn resolve_root(root: &Path, home: bool) -> Resolved {
    let canon = if home {
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

/// True if walking `root` (of `kind`) reads the directory `p`: `p` is `root`
/// or lies under it within the walk depth, and for the home root under a dot
/// entry.
fn walks_into(root: &Path, kind: RootKind, p: &Path) -> bool {
    let Ok(rel) = p.strip_prefix(root) else {
        return false;
    };
    let mut comps = rel.components();
    let Some(first) = comps.next() else {
        return true;
    };
    if kind == RootKind::Home && !is_dot(first.as_os_str()) {
        return false;
    }
    rel.components().count() <= MAX_DEPTH
}

/// Every depth-1 child of each root is a unit, except under the `Home` root,
/// where only dot entries are. Each root's walk leaves out the subtrees of
/// every other root and of every `excluded` directory. An `Extra` root that
/// another root (or an excluded directory) already covers is not walked and is
/// recorded in `skipped` instead.
pub fn scan(roots: &[Root], excluded: &[PathBuf]) -> WalkResult {
    let mut result = WalkResult::default();
    let resolved: Vec<Resolved> = roots
        .iter()
        .map(|r| resolve_root(&r.path, r.kind == RootKind::Home))
        .collect();
    let canon = |i: usize| match &resolved[i] {
        Resolved::Dir(p) | Resolved::Symlink(p) => Some(p.clone()),
        _ => None,
    };

    // Decide which roots are walked. Extra roots are checked shortest path
    // first, so any root that could cover one has already been decided.
    let mut active: Vec<usize> = (0..roots.len())
        .filter(|&i| roots[i].kind != RootKind::Extra)
        .collect();
    let mut extras: Vec<(usize, PathBuf)> = (0..roots.len())
        .filter(|&i| roots[i].kind == RootKind::Extra)
        .map(|i| (i, canon(i).unwrap_or_else(|| roots[i].path.clone())))
        .collect();
    extras.sort_by_key(|(i, p)| (p.components().count(), *i));
    let mut skipped: Vec<(usize, SkippedRoot)> = Vec::new();
    for (i, p) in extras {
        // The closest enclosing claimed path decides: a root that is not a
        // walked directory leaves its subtree to nobody.
        let walked_dir = |j: usize| matches!(resolved[j], Resolved::Dir(_));
        let enclosing_root = active
            .iter()
            .filter_map(|&j| canon(j).map(|c| (j, c)))
            .filter(|(_, c)| p.starts_with(c))
            .max_by_key(|(_, c)| c.components().count());
        let enclosing_excluded = excluded
            .iter()
            .filter(|e| p.starts_with(e))
            .max_by_key(|e| e.components().count());
        let excl_depth = enclosing_excluded.map(|e| e.components().count());
        let root_depth = enclosing_root.as_ref().map(|(_, c)| c.components().count());
        let cover = match (enclosing_excluded, enclosing_root) {
            (Some(e), _) if excl_depth >= root_depth => Some(SkippedRoot {
                root: roots[i].path.clone(),
                covered_by: e.clone(),
                dotfiles: true,
            }),
            (_, Some((j, c))) if walked_dir(j) && walks_into(&c, roots[j].kind, &p) => {
                Some(SkippedRoot {
                    root: roots[i].path.clone(),
                    covered_by: c,
                    dotfiles: false,
                })
            }
            _ => None,
        };
        match cover {
            Some(s) => skipped.push((i, s)),
            None => active.push(i),
        }
    }
    active.sort_unstable();
    skipped.sort_by_key(|(i, _)| *i);
    result.skipped = skipped.into_iter().map(|(_, s)| s).collect();

    // Paths every walk leaves out: the other roots and the excluded dirs.
    let mut claimed: Vec<PathBuf> = active.iter().filter_map(|&i| canon(i)).collect();
    claimed.extend(excluded.iter().cloned());
    for i in active {
        match &resolved[i] {
            Resolved::Dir(c) => {
                let prune: Vec<PathBuf> = claimed.iter().filter(|p| *p != c).cloned().collect();
                scan_root(c, roots[i].kind == RootKind::Home, &prune, &mut result);
            }
            Resolved::Symlink(p) => result.symlinks.push(p.clone()),
            Resolved::Skip => {}
            Resolved::Unreadable => result.unreadable.push(roots[i].path.clone()),
        }
    }
    result.units.sort_by(|a, b| a.path.cmp(&b.path));
    result.unreadable.sort();
    result.symlinks.sort();
    result
}

fn scan_root(root: &Path, dot_only: bool, prune: &[PathBuf], result: &mut WalkResult) {
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
        if prune.contains(&path) {
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
            walk_dir(&path, 1, prune, &mut files, result);
            files.sort();
            result.units.push(UnitFiles { path, files });
        }
        // sockets, FIFOs and devices are skipped without being opened
    }
}

fn walk_dir(
    dir: &Path,
    depth: usize,
    prune: &[PathBuf],
    files: &mut Vec<PathBuf>,
    result: &mut WalkResult,
) {
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
        if prune.contains(&path) {
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
            files.push(path);
        } else if ft.is_dir() && depth < MAX_DEPTH {
            walk_dir(&path, depth + 1, prune, files, result);
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
