#![allow(dead_code)]

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Config,
    State,
    Cache,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    Tracked,
    TrackedDrifted,
    TrackedUnverified,
    PartiallyTracked,
    Untracked,
}

/// A secret finding. Deliberately has no field that can hold the secret value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub path: PathBuf,
    pub line: usize,
    pub committed: bool,
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub path: PathBuf,
    pub app: Option<String>,
    pub kind: Kind,
    pub tracking: Tracking,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub units: Vec<Unit>,
    pub unreadable: Vec<PathBuf>,
    pub ignored_by_file: usize,
    pub cache_state_count: usize,
    pub committed_findings: Vec<Finding>,
}
