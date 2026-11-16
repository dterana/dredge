use crate::cli::FailOn;
use crate::model::{Report, Tracking};

/// Returns 1 if any selected category has findings, else 0.
/// Exit code 2 (tool failure) is produced by the caller.
pub fn code(report: &Report, fail_on: &[FailOn]) -> i32 {
    let hit = fail_on.iter().any(|category| match category {
        FailOn::Secrets => {
            !report.committed_findings.is_empty()
                || report.units.iter().any(|u| !u.findings.is_empty())
        }
        FailOn::Drift => report
            .units
            .iter()
            .any(|u| u.tracking == Tracking::TrackedDrifted || !u.drifted.is_empty()),
        FailOn::Untracked => report.units.iter().any(|u| {
            !u.ignored && matches!(u.tracking, Tracking::Untracked | Tracking::PartiallyTracked)
        }),
    });
    i32::from(hit)
}
