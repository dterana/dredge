use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cli::LayoutArg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Stow,
    Chezmoi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub repo_path: PathBuf,
    pub unverified: bool,
    pub template: bool,
}

#[derive(Debug, Default)]
pub struct Index {
    entries: BTreeMap<PathBuf, RepoEntry>,
    /// Subdirectories of the dotfiles dir that could not be read.
    unreadable: Vec<PathBuf>,
}

pub fn detect(dir: &Path, arg: LayoutArg) -> Layout {
    match arg {
        LayoutArg::Stow => Layout::Stow,
        LayoutArg::Chezmoi => Layout::Chezmoi,
        LayoutArg::Auto => {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return Layout::Stow;
            };
            let is_chezmoi = rd.flatten().any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name == ".chezmoiroot" || name == ".chezmoiignore" || name.starts_with("dot_")
            });
            if is_chezmoi {
                Layout::Chezmoi
            } else {
                Layout::Stow
            }
        }
    }
}

/// Sorted children of `dir` with their type, without following symlinks.
fn children(dir: &Path) -> std::io::Result<Vec<(String, PathBuf, std::fs::FileType)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let ft = e.file_type()?;
        out.push((e.file_name().to_string_lossy().into_owned(), e.path(), ft));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Children of a subdirectory; a failure records `dir` as unreadable and
/// yields nothing, so the rest of the index is still built.
fn sub_children(
    dir: &Path,
    unreadable: &mut Vec<PathBuf>,
) -> Vec<(String, PathBuf, std::fs::FileType)> {
    children(dir).unwrap_or_else(|_| {
        unreadable.push(dir.to_path_buf());
        Vec::new()
    })
}

/// Stow control files: at the top of a package stow reads them and never
/// links them, so they are not targets.
const STOW_CONTROL_FILES: &[&str] = &[".stow-local-ignore", ".stow-global-ignore"];

/// Walk a stow package, mapping paths relative to the package root.
fn walk_stow(
    dir: &Path,
    rel: &Path,
    entries: &mut BTreeMap<PathBuf, RepoEntry>,
    unreadable: &mut Vec<PathBuf>,
) {
    for (name, path, ft) in sub_children(dir, unreadable) {
        if name == ".git" {
            continue;
        }
        let child_rel = rel.join(&name);
        if ft.is_dir() {
            walk_stow(&path, &child_rel, entries, unreadable);
        } else if ft.is_file() {
            if rel.as_os_str().is_empty() && STOW_CONTROL_FILES.contains(&name.as_str()) {
                continue;
            }
            entries.entry(child_rel).or_insert(RepoEntry {
                repo_path: path,
                unverified: false,
                template: false,
            });
        }
    }
}

/// Result of decoding one chezmoi source path component.
struct Decoded {
    name: String,
    unverified: bool,
    template: bool,
    skip: bool,
}

/// Decode chezmoi attributes. Supported: dot_, private_, executable_,
/// readonly_, symlink_, .tmpl. exact_ and encrypted_ and create_ are
/// stripped but mark the entry unverified; run_ and modify_ (and create_
/// files) are scripts with no sensible target and are skipped.
fn decode(name: &str, is_file: bool) -> Decoded {
    let mut rest = name;
    let mut d = Decoded {
        name: String::new(),
        unverified: false,
        template: false,
        skip: false,
    };
    loop {
        if let Some(r) = rest.strip_prefix("dot_") {
            rest = r;
            d.name.push('.');
            // dot_ is always last in chezmoi's ordering for its own component
            break;
        }
        let mut matched = false;
        for p in ["private_", "executable_", "readonly_"] {
            if let Some(r) = rest.strip_prefix(p) {
                rest = r;
                matched = true;
            }
        }
        for p in ["symlink_", "exact_", "encrypted_"] {
            if let Some(r) = rest.strip_prefix(p) {
                rest = r;
                d.unverified = true;
                matched = true;
            }
        }
        for p in ["run_", "modify_", "create_"] {
            if rest.starts_with(p) && is_file {
                d.skip = true;
            }
            if let Some(r) = rest.strip_prefix(p) {
                rest = r;
                d.unverified = true;
                matched = true;
            }
        }
        if !matched {
            break;
        }
    }
    if is_file {
        if let Some(r) = rest.strip_suffix(".tmpl") {
            rest = r;
            d.template = true;
        }
    }
    d.name.push_str(rest);
    d
}

fn walk_chezmoi(
    dir: &Path,
    rel: &Path,
    inherited_unverified: bool,
    entries: &mut BTreeMap<PathBuf, RepoEntry>,
    unreadable: &mut Vec<PathBuf>,
) {
    for (name, path, ft) in sub_children(dir, unreadable) {
        // chezmoi ignores source names starting with "." (including .git)
        if name.starts_with('.') {
            continue;
        }
        if ft.is_dir() {
            let d = decode(&name, false);
            walk_chezmoi(
                &path,
                &rel.join(d.name),
                inherited_unverified || d.unverified,
                entries,
                unreadable,
            );
        } else if ft.is_file() {
            let d = decode(&name, true);
            if d.skip || d.name.is_empty() {
                continue;
            }
            entries.entry(rel.join(&d.name)).or_insert(RepoEntry {
                repo_path: path,
                unverified: inherited_unverified || d.unverified,
                template: d.template,
            });
        }
    }
}

/// Real chezmoi source root. `.chezmoiroot` is honored only when it is a
/// small regular file (not a symlink, FIFO, etc.) naming a plain relative
/// subdirectory that is a real directory (not a symlink). On any violation
/// it is ignored and `dir` itself is indexed.
fn chezmoi_root(dir: &Path) -> PathBuf {
    const MAX_ROOT_FILE: u64 = 4096;
    let fallback = || dir.to_path_buf();
    let file = dir.join(".chezmoiroot");
    match std::fs::symlink_metadata(&file) {
        Ok(m) if m.file_type().is_file() && m.len() <= MAX_ROOT_FILE => {}
        _ => return fallback(),
    }
    let Ok(Some(f)) = crate::secrets::open_regular(&file) else {
        return fallback();
    };
    let mut buf = Vec::new();
    {
        use std::io::Read;
        if f.take(MAX_ROOT_FILE + 1).read_to_end(&mut buf).is_err()
            || buf.len() as u64 > MAX_ROOT_FILE
        {
            return fallback();
        }
    }
    let Ok(text) = String::from_utf8(buf) else {
        return fallback();
    };
    let sub = Path::new(text.trim());
    let plain = !text.trim().is_empty()
        && sub
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    // Every accumulated prefix must be a real directory (no symlinks).
    let mut target = dir.to_path_buf();
    let mut real_dir = plain;
    if plain {
        for comp in sub.components() {
            target.push(comp);
            real_dir = std::fs::symlink_metadata(&target)
                .map(|m| m.file_type().is_dir())
                .unwrap_or(false);
            if !real_dir {
                break;
            }
        }
    }
    if real_dir {
        target
    } else {
        fallback()
    }
}

/// Most entries the stow-directory hint reads from one directory; a larger
/// directory is left alone.
const HINT_MAX_ENTRIES: usize = 1024;

/// Most subdirectories of one candidate the hint looks inside.
const HINT_MAX_PROBES: usize = 32;

/// Children of `dir` with their type, without following symlinks, sorted by
/// name. `None` when `dir` cannot be read or has more than `HINT_MAX_ENTRIES`.
fn bounded_children(dir: &Path) -> Option<Vec<(String, std::fs::FileType)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).ok()? {
        if out.len() == HINT_MAX_ENTRIES {
            return None;
        }
        let e = e.ok()?;
        let ft = e.file_type().ok()?;
        out.push((e.file_name().to_string_lossy().into_owned(), ft));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// Dot-prefixed names that version control, Finder and tooling put in any
/// folder. They neither make a folder a package nor stop one from looking
/// like a stow directory.
const HINT_METADATA: &[&str] = &[
    ".DS_Store",
    ".git",
    ".gitignore",
    ".gitattributes",
    ".gitmodules",
    ".gitkeep",
    ".github",
];

/// A dot-prefixed name (file, directory or symlink) that a package would
/// hold: anything starting with a dot except `HINT_METADATA`.
fn is_dot_entry(name: &str) -> bool {
    name.starts_with('.') && !HINT_METADATA.contains(&name)
}

/// True if `dir` looks like a stow directory of packages rather than a
/// package: no dot entry at its own top level, and at least two real
/// subdirectories that each hold a dot entry.
fn looks_like_stow_dir(dir: &Path) -> bool {
    let Some(children) = bounded_children(dir) else {
        return false;
    };
    if children.iter().any(|(n, _)| is_dot_entry(n)) {
        return false;
    }
    let packages = children
        .iter()
        .filter(|(_, ft)| ft.is_dir())
        .take(HINT_MAX_PROBES)
        .filter(|(n, _)| {
            bounded_children(&dir.join(n))
                .is_some_and(|sub| sub.iter().any(|(n, _)| is_dot_entry(n)))
        })
        .take(2)
        .count();
    packages >= 2
}

/// Top-level directories of a stow repo `dir` that look like stow
/// directories themselves (their subfolders look like packages), sorted.
/// Symlinks are never followed and every directory read is bounded.
pub fn stow_dir_hints(dir: &Path) -> Vec<PathBuf> {
    let Some(top) = bounded_children(dir) else {
        return Vec::new();
    };
    top.into_iter()
        .filter(|(n, ft)| ft.is_dir() && !n.starts_with('.'))
        .map(|(n, _)| dir.join(n))
        .filter(|p| looks_like_stow_dir(p))
        .collect()
}

impl Index {
    /// Fails only when `dir` itself cannot be read; an unreadable
    /// subdirectory is recorded in [`Index::unreadable`] and skipped.
    pub fn build(dir: &Path, layout: Layout) -> std::io::Result<Index> {
        let mut entries = BTreeMap::new();
        let mut unreadable = Vec::new();
        match layout {
            Layout::Stow => {
                for (name, path, ft) in children(dir)? {
                    if name.starts_with('.') || !ft.is_dir() {
                        continue;
                    }
                    walk_stow(&path, Path::new(""), &mut entries, &mut unreadable);
                }
            }
            Layout::Chezmoi => {
                // Fail early on a missing, unreadable or non-directory dir.
                children(dir)?;
                walk_chezmoi(
                    &chezmoi_root(dir),
                    Path::new(""),
                    false,
                    &mut entries,
                    &mut unreadable,
                );
            }
        }
        unreadable.sort();
        Ok(Index {
            entries,
            unreadable,
        })
    }

    pub fn unreadable(&self) -> &[PathBuf] {
        &self.unreadable
    }

    pub fn lookup(&self, home_rel: &Path) -> Option<RepoEntry> {
        self.entries.get(home_rel).cloned()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&PathBuf, &RepoEntry)> {
        self.entries.iter()
    }
}
