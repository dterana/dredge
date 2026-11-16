use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use regex::bytes::{Regex, RegexSet};
use serde::Deserialize;

use crate::model::Finding;

const EMBEDDED: &str = include_str!("../rules/secrets.toml");

pub const MAX_SCAN_BYTES: u64 = 1_048_576;

/// One secret rule as written in the rules file, including disabled rules.
#[derive(Debug, Deserialize)]
pub struct RuleDef {
    pub id: String,
    pub regex: String,
    pub enabled: bool,
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default)]
    rule: Vec<RuleDef>,
}

#[derive(Debug, Default)]
pub struct SecretRules {
    ids: Vec<String>,
    regexes: Vec<Regex>,
    set: Option<RegexSet>,
}

impl SecretRules {
    /// Parses the embedded rules. Falls back to empty rules on error;
    /// a test asserts the embedded file parses.
    pub fn embedded() -> SecretRules {
        SecretRules::parse(EMBEDDED).unwrap_or_default()
    }

    /// Parses rule definitions without compiling them. Used by schema tests.
    pub fn parse_defs(text: &str) -> Result<Vec<RuleDef>, String> {
        let raw: RawFile = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(raw.rule)
    }

    pub fn parse(text: &str) -> Result<SecretRules, String> {
        let raw: RawFile = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        let mut regexes = Vec::new();
        let mut patterns = Vec::new();
        for r in raw.rule.into_iter().filter(|r| r.enabled) {
            let re = Regex::new(&r.regex).map_err(|e| format!("rule {}: {e}", r.id))?;
            ids.push(r.id);
            patterns.push(r.regex);
            regexes.push(re);
        }
        let set = if patterns.is_empty() {
            None
        } else {
            Some(RegexSet::new(&patterns).map_err(|e| e.to_string())?)
        };
        Ok(SecretRules { ids, regexes, set })
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Opens without following symlinks and without blocking on FIFOs, then checks
/// the opened handle is a regular file. Returns None for anything else,
/// including a path swapped to a symlink or FIFO after the metadata check.
pub fn open_regular(path: &Path) -> std::io::Result<Option<File>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENXIO)) => {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    Ok(Some(file))
}

/// Scans one file. Findings carry only rule id, path and 1-based line.
pub fn scan_file(path: &Path, rules: &SecretRules) -> std::io::Result<Vec<Finding>> {
    let Some(set) = &rules.set else {
        return Ok(Vec::new());
    };
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > MAX_SCAN_BYTES {
        return Ok(Vec::new());
    }
    let Some(file) = open_regular(path)? else {
        return Ok(Vec::new());
    };
    let mut buf = Vec::new();
    file.take(MAX_SCAN_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_SCAN_BYTES || buf.contains(&0) {
        return Ok(Vec::new());
    }

    let mut findings = Vec::new();
    for (idx, line) in buf.split(|&b| b == b'\n').enumerate() {
        for rule_idx in set.matches(line).iter() {
            for _ in rules.regexes[rule_idx].find_iter(line) {
                findings.push(Finding {
                    rule_id: rules.ids[rule_idx].clone(),
                    path: path.to_path_buf(),
                    line: idx + 1,
                    committed: false,
                });
            }
        }
    }
    Ok(findings)
}
