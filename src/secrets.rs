use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use regex::bytes::{Regex, RegexSet};
use serde::Deserialize;

use crate::model::Finding;

const EMBEDDED: &str = include_str!("../rules/secrets.toml");

pub const MAX_SCAN_BYTES: u64 = 1_048_576;

/// How much of a file is read before deciding whether it is binary. Most
/// large files in dot directories are binary, so this avoids reading them whole.
pub const BINARY_PROBE_BYTES: usize = 8192;

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

/// Reads at most `BINARY_PROBE_BYTES` from `r`. Returns whether the chunk
/// contains a NUL byte, and the chunk itself.
pub fn probe_is_binary<R: Read>(r: &mut R) -> io::Result<(bool, Vec<u8>)> {
    let mut chunk = Vec::new();
    r.take(BINARY_PROBE_BYTES as u64).read_to_end(&mut chunk)?;
    let binary = chunk.contains(&0);
    Ok((binary, chunk))
}

/// Appends the rest of `r` to `buf`, reading at most one byte past
/// `MAX_SCAN_BYTES` in total. Returns true when the content exceeds the cap,
/// which also covers a file that grew after its size was checked.
pub fn read_capped<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<bool> {
    let remaining = (MAX_SCAN_BYTES + 1).saturating_sub(buf.len() as u64);
    r.take(remaining).read_to_end(buf)?;
    Ok(buf.len() as u64 > MAX_SCAN_BYTES)
}

/// Why a file was not scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Over `MAX_SCAN_BYTES`.
    TooLarge,
    /// Contains a NUL byte.
    Binary,
    /// Not a regular file (symlink, FIFO, device) or gone before it was opened.
    NotRegular,
}

/// Findings plus the reason the file was skipped, if it was.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub findings: Vec<Finding>,
    pub skipped: Option<SkipReason>,
}

fn skipped(reason: SkipReason) -> io::Result<ScanOutcome> {
    Ok(ScanOutcome {
        findings: Vec::new(),
        skipped: Some(reason),
    })
}

/// Scans one file. Findings carry only rule id, path and 1-based line.
pub fn scan_file(path: &Path, rules: &SecretRules) -> io::Result<Vec<Finding>> {
    scan_file_detailed(path, rules).map(|o| o.findings)
}

/// Like `scan_file`, and also says why a file was skipped.
pub fn scan_file_detailed(path: &Path, rules: &SecretRules) -> io::Result<ScanOutcome> {
    if rules.set.is_none() {
        return Ok(ScanOutcome::default());
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return skipped(SkipReason::NotRegular);
    }
    if meta.len() > MAX_SCAN_BYTES {
        return skipped(SkipReason::TooLarge);
    }
    let Some(file) = open_regular(path)? else {
        return skipped(SkipReason::NotRegular);
    };
    let mut file = file;
    scan_reader(&mut file, path, rules)
}

/// Scans an already opened reader: probes the first chunk for NUL, reads the
/// rest up to the cap, then scans lines. `path` only labels the findings.
pub fn scan_reader<R: Read>(
    r: &mut R,
    path: &Path,
    rules: &SecretRules,
) -> io::Result<ScanOutcome> {
    let Some(set) = &rules.set else {
        return Ok(ScanOutcome::default());
    };
    let (binary, mut buf) = probe_is_binary(r)?;
    if binary {
        return skipped(SkipReason::Binary);
    }
    if read_capped(r, &mut buf)? {
        return skipped(SkipReason::TooLarge);
    }
    if buf.contains(&0) {
        return skipped(SkipReason::Binary);
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
    Ok(ScanOutcome {
        findings,
        skipped: None,
    })
}
