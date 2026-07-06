//! Finding/report types shared across checks, stores, and the CLI.

use serde::{Deserialize, Serialize};

/// A candidate anomaly surfaced by a [`crate::checks::Check`], *before* it is
/// reconciled against prior-scan state. Keyed by `path` — the reconcile step
/// (baseline design §4) treats `path` as the finding identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anomaly {
    /// Stable identity of the finding. For file surfaces this is a real
    /// path; for non-file surfaces it is a synthetic but stable key
    /// (e.g. `flatpak-override:org.foo/filesystem=host`,
    /// `listener:tcp:0.0.0.0:8080`).
    pub path: String,
    /// Which check produced this (`systemd-user`, `shell-rc`, …).
    pub check: String,
    /// Human-readable explanation, stored in the `findings.detail` column.
    pub detail: String,
}

impl Anomaly {
    pub fn new(check: &str, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            check: check.to_string(),
            detail: detail.into(),
        }
    }
}

/// The three diff buckets from baseline design §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// Present now, not in the prior open set.
    New,
    /// Present now and in the prior open set.
    StillOutstanding,
    /// Prior open finding no longer present (reverted to expected).
    Resolved,
}

/// One reconciled finding as reported to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedFinding {
    pub finding_id: i64,
    pub path: String,
    pub detail: String,
    pub status: FindingStatus,
}

/// The report shown to the user: three buckets, never a flat re-dump
/// (baseline design §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    pub scan_id: i64,
    pub tier: String,
    pub deployment_checksum: String,
    /// Surfaces whose per-surface baseline was captured for the first time
    /// this run (trust-on-first-use); reported for transparency, not as
    /// findings.
    #[serde(default)]
    pub baselined_surfaces: Vec<String>,
    /// Checks that could not run because a required host tool was absent
    /// (e.g. `flatpak` not installed) — surfaced so a clean report isn't
    /// mistaken for "nothing to see" when a check was simply skipped.
    #[serde(default)]
    pub skipped_checks: Vec<String>,
    /// Checks deliberately deferred to a future release (currently only the
    /// outbound-connections check awaiting Component A's connection log).
    #[serde(default)]
    pub deferred_checks: Vec<String>,
    pub new: Vec<ReportedFinding>,
    pub still_outstanding: Vec<ReportedFinding>,
    pub resolved: Vec<ReportedFinding>,
}

impl ScanReport {
    /// Total count of actionable findings (new + still outstanding).
    pub fn outstanding_count(&self) -> usize {
        self.new.len() + self.still_outstanding.len()
    }

    /// True when the scan surfaced no new or still-outstanding anomalies.
    pub fn is_clean(&self) -> bool {
        self.outstanding_count() == 0
    }
}
