use std::fs;
use std::io::Read;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::model::Report;
use crate::secrets::open_regular;

const MAX_IGNORE_BYTES: u64 = 64 * 1024;
const PROBE: &str = "__dredge_probe__";

#[derive(Debug, Clone, Default)]
pub struct IgnoreList {
    set: Option<GlobSet>,
}

impl IgnoreList {
    /// Loads `.dredgeignore` from the dotfiles root. Anything unusual yields an empty list.
    pub fn load(dotfiles: &Path) -> IgnoreList {
        let path = dotfiles.join(".dredgeignore");
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() && m.len() <= MAX_IGNORE_BYTES => {}
            _ => return IgnoreList::default(),
        }
        let Ok(Some(file)) = open_regular(&path) else {
            return IgnoreList::default();
        };
        let mut text = String::new();
        if file
            .take(MAX_IGNORE_BYTES + 1)
            .read_to_string(&mut text)
            .is_err()
            || text.len() as u64 > MAX_IGNORE_BYTES
        {
            return IgnoreList::default();
        }
        IgnoreList::from_text(&text)
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

    /// Drops ignored units without findings and flags ignored units that still carry findings.
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
            if unit.findings.is_empty() {
                false
            } else {
                unit.ignored = true;
                true
            }
        });
        report.ignored_by_file += counted;
    }
}
