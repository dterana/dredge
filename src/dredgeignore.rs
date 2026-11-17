use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::model::{Report, Tracking};
use crate::secrets::open_regular;

const MAX_IGNORE_BYTES: u64 = 64 * 1024;
const PROBE: &str = "__dredge_probe__";

/// The text of `dotfiles/.dredgeignore`: a regular file of at most
/// `MAX_IGNORE_BYTES`, opened without following symlinks, else `None`.
fn read_file(dotfiles: &Path) -> Option<String> {
    let path = dotfiles.join(".dredgeignore");
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() && m.len() <= MAX_IGNORE_BYTES => {}
        _ => return None,
    }
    let Ok(Some(file)) = open_regular(&path) else {
        return None;
    };
    let mut text = String::new();
    if file
        .take(MAX_IGNORE_BYTES + 1)
        .read_to_string(&mut text)
        .is_err()
        || text.len() as u64 > MAX_IGNORE_BYTES
    {
        return None;
    }
    Some(text)
}

#[derive(Debug, Clone, Default)]
pub struct IgnoreList {
    set: Option<GlobSet>,
}

impl IgnoreList {
    /// Loads `.dredgeignore` from the dotfiles root. Anything unusual yields an empty list.
    pub fn load(dotfiles: &Path) -> IgnoreList {
        IgnoreList::from_text(&read_file(dotfiles).unwrap_or_default())
    }

    /// The union of the `.dredgeignore` files at the root of each directory.
    /// Each file is read on its own terms; a missing or unusual one adds nothing.
    pub fn load_all(dirs: &[PathBuf]) -> IgnoreList {
        let texts: Vec<String> = dirs.iter().filter_map(|d| read_file(d)).collect();
        IgnoreList::from_text(&texts.join("\n"))
    }

    fn from_text(text: &str) -> IgnoreList {
        let mut builder = GlobSetBuilder::new();
        let mut any = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Ok(glob) = Glob::new(line) {
                builder.add(glob);
                any = true;
            }
        }
        if !any {
            return IgnoreList::default();
        }
        IgnoreList {
            set: builder.build().ok(),
        }
    }

    /// True if the path, any ancestor, or (for `dir/**` globs) the directory itself matches.
    pub fn is_ignored(&self, home_rel: &Path) -> bool {
        let Some(set) = &self.set else {
            return false;
        };
        home_rel
            .ancestors()
            .any(|a| !a.as_os_str().is_empty() && (set.is_match(a) || set.is_match(a.join(PROBE))))
    }

    /// Ignoring only removes a unit from the untracked category: ignored units
    /// with no findings and no drift are dropped, the rest are kept and
    /// flagged so their secrets and drift are still reported.
    pub fn apply(&self, report: &mut Report, home: &Path) {
        if self.set.is_none() {
            return;
        }
        let mut counted = 0;
        report.units.retain_mut(|unit| {
            let Ok(rel) = unit.path.strip_prefix(home) else {
                return true;
            };
            if !self.is_ignored(rel) {
                return true;
            }
            counted += 1;
            let drifted = unit.tracking == Tracking::TrackedDrifted || !unit.drifted.is_empty();
            if unit.findings.is_empty() && !drifted {
                false
            } else {
                unit.ignored = true;
                true
            }
        });
        report.ignored_by_file += counted;
    }
}
