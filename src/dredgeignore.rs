use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::model::{Report, Tracking};
use crate::secrets::open_regular;

const MAX_IGNORE_BYTES: u64 = 64 * 1024;
const PROBE: &str = "__dredge_probe__";

/// The text of `dotfiles/.dredgeignore`: a regular file of at most
/// `MAX_IGNORE_BYTES`, opened without following symlinks, else `None`.
fn read_file(dotfiles: &Path) -> Option<String> {
    read_named(dotfiles, ".dredgeignore")
}

/// The text of `dir/name`, under the same limits as `read_file`.
fn read_named(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
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
    /// True when at least one pattern came from a `.chezmoiignore`.
    chezmoi: bool,
    /// The `.chezmoiignore` patterns, matched with `*` not crossing `/`.
    chezmoi_set: Option<GlobSet>,
}

/// The patterns of a `.chezmoiignore` that dredge can honor without running
/// chezmoi's template engine. Blank lines, `#` comments and `!` negations are
/// skipped, and so is every line that holds a template action or sits inside
/// an `if`, `range` or `with` block, since whether it applies depends on the
/// machine. Skipping a line only means more is reported as untracked. A
/// leading or trailing `/` is dropped.
fn chezmoi_patterns(text: &str) -> Vec<String> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.contains("{{") {
            let opens = ["if", "range", "with"].iter().any(|kw| {
                line.split("{{")
                    .skip(1)
                    .any(|a| a.trim_start_matches('-').trim_start().starts_with(kw))
            });
            let ends = line
                .split("{{")
                .skip(1)
                .any(|a| a.trim_start_matches('-').trim_start().starts_with("end"));
            if opens {
                depth += 1;
            } else if ends {
                depth = depth.saturating_sub(1);
            }
            continue;
        }
        if depth > 0 || line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let line = line.trim_start_matches('/').trim_end_matches('/');
        if !line.is_empty() {
            out.push(line.to_string());
        }
    }
    out
}

impl IgnoreList {
    /// Loads `.dredgeignore` from the dotfiles root. Anything unusual yields an empty list.
    pub fn load(dotfiles: &Path) -> IgnoreList {
        IgnoreList::from_text(&read_file(dotfiles).unwrap_or_default())
    }

    /// The union of the `.dredgeignore` files at the root of each directory.
    /// Each file is read on its own terms; a missing or unusual one adds nothing.
    pub fn load_all(dirs: &[PathBuf]) -> IgnoreList {
        IgnoreList::load_all_with_chezmoi(dirs, &[])
    }

    /// `load_all`, plus the `.chezmoiignore` found in the source root of each
    /// of `chezmoi_dirs` (the dotfiles directories read as chezmoi sources;
    /// `.chezmoiroot` is honored). Its patterns name target paths relative to
    /// the home directory, and `*` does not cross `/`, as in chezmoi.
    pub fn load_all_with_chezmoi(dirs: &[PathBuf], chezmoi_dirs: &[PathBuf]) -> IgnoreList {
        let texts: Vec<String> = dirs.iter().filter_map(|d| read_file(d)).collect();
        let mut list = IgnoreList::from_text(&texts.join("\n"));
        let patterns: Vec<String> = chezmoi_dirs
            .iter()
            .filter_map(|d| read_named(&crate::layout::chezmoi_root(d), ".chezmoiignore"))
            .flat_map(|t| chezmoi_patterns(&t))
            .collect();
        let mut builder = GlobSetBuilder::new();
        let mut any = false;
        for pat in &patterns {
            if let Ok(g) = GlobBuilder::new(pat).literal_separator(true).build() {
                builder.add(g);
                any = true;
            }
        }
        if any {
            if let Ok(extra) = builder.build() {
                list.chezmoi_set = Some(extra);
                list.chezmoi = true;
            }
        }
        list
    }

    /// True if a `.chezmoiignore` pattern was loaded.
    pub fn has_chezmoi_patterns(&self) -> bool {
        self.chezmoi
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
            ..IgnoreList::default()
        }
    }

    /// True if the path, any ancestor, or (for `dir/**` globs) the directory itself matches.
    pub fn is_ignored(&self, home_rel: &Path) -> bool {
        let sets = [&self.set, &self.chezmoi_set];
        home_rel.ancestors().any(|a| {
            !a.as_os_str().is_empty()
                && sets
                    .iter()
                    .filter_map(|s| s.as_ref())
                    .any(|set| set.is_match(a) || set.is_match(a.join(PROBE)))
        })
    }

    /// Ignoring only removes a unit from the untracked category: ignored units
    /// with no findings and no drift are dropped, the rest are kept and
    /// flagged so their secrets and drift are still reported.
    pub fn apply(&self, report: &mut Report, home: &Path) {
        if self.set.is_none() && self.chezmoi_set.is_none() {
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

#[cfg(test)]
mod chezmoi_tests {
    use super::chezmoi_patterns;

    #[test]
    fn plain_lines_are_kept_and_normalized() {
        assert_eq!(
            chezmoi_patterns("# c\n\n.a\n/.b/\n!.c\n  .d  \n"),
            vec![".a", ".b", ".d"]
        );
    }

    #[test]
    fn template_actions_and_their_blocks_are_skipped() {
        let text = "{{ if eq .chezmoi.os \"darwin\" }}\n.mac\n{{ else }}\n.other\n{{ end }}\n.after\n{{- with .x }}\n.w\n{{- end }}\n.last\n";
        assert_eq!(chezmoi_patterns(text), vec![".after", ".last"]);
    }
}
