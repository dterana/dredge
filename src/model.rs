use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Config,
    State,
    Cache,
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Config => "config",
            Kind::State => "state",
            Kind::Cache => "cache",
            Kind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    Tracked,
    TrackedDrifted,
    TrackedUnverified,
    PartiallyTracked,
    Untracked,
}

impl Tracking {
    pub fn as_str(self) -> &'static str {
        match self {
            Tracking::Tracked => "tracked",
            Tracking::TrackedDrifted => "tracked-drifted",
            Tracking::TrackedUnverified => "tracked-unverified",
            Tracking::PartiallyTracked => "partially-tracked",
            Tracking::Untracked => "untracked",
        }
    }
}

/// A secret finding. Deliberately has no field that can hold the secret value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub rule_id: String,
    pub path: PathBuf,
    pub line: usize,
    pub committed: bool,
}

/// A home file whose content differs from its repo copy. Paths only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub path: PathBuf,
    pub repo_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Unit {
    pub path: PathBuf,
    pub app: Option<String>,
    pub kind: Kind,
    pub tracking: Tracking,
    pub findings: Vec<Finding>,
    /// Matched by `.dredgeignore`. Kept in the report only because it has
    /// findings or drift.
    pub ignored: bool,
    /// The unit root itself is a symlink (listed, never followed).
    pub symlink: bool,
    /// Config or unknown files in this unit whose content differs from the repo.
    pub drifted: Vec<Drift>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Canonical home directory; used to display paths as `~/...`.
    pub home: PathBuf,
    /// Canonical dotfiles directories, in the order given (deduplicated), or
    /// the detected chezmoi source dir. Empty when there is none.
    pub dotfiles: Vec<PathBuf>,
    pub units: Vec<Unit>,
    pub unreadable: Vec<PathBuf>,
    pub ignored_by_file: usize,
    pub cache_state_count: usize,
    /// Directories not entered because they sit at the walk depth cap.
    pub depth_truncated: usize,
    /// Cache or state directories the walk did not enter (outermost only).
    pub pruned_dirs: usize,
    /// Config or unknown files over the scan size cap, not scanned for secrets.
    pub skipped_too_large: usize,
    pub committed_findings: Vec<Finding>,
    /// Home-relative paths present in the repo but missing from home.
    pub missing_on_machine: Vec<PathBuf>,
    /// One-line notices for stderr (for example a skipped `--root`). Not part
    /// of the human or JSON report.
    pub notes: Vec<String>,
}
