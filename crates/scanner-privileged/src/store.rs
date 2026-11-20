//! `scans.db` — scan history and findings store.
//!
//! Schema follows the baseline design spec
//! (`docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` §4)
//! `scan_runs`/`findings` tables, plus the privileged spec's (§4)
//! `check_type` discriminator column. The baseline doc phrases the column as
//! an `ALTER TABLE findings ADD COLUMN check_type ...` because it assumes the
//! table already exists from Phase 5; Phase 5's crate is a sibling branch not
//! yet merged into this worktree, so we CREATE the table with `check_type`
//! already present and consistent with that spec.
//!
//! RECONCILIATION NOTE: Phase 5's userspace-tier crate will define its own
//! `scans.db` store against the same spec. When both branches merge, these
//! two stores MUST be reconciled into one shared schema/crate (a
//! `scanner-store` crate is the obvious home). Until then this is the
//! canonical privileged-tier definition, kept deliberately schema-compatible
//! so the merge is a lift-and-share, not a rewrite.
//!
//! Only `anomalous` findings are persisted as reconciled rows (per the
//! baseline diff logic — `expected`/`informational` states are not stored
//! here). Lockdown/Secure-Boot *transitions* are detected via the separate
//! `state_snapshots` table, not via persisted findings.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Result, ScanError};
use crate::model::{CheckType, Classification, Finding};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS scan_runs (
    scan_id              INTEGER PRIMARY KEY,
    started_at           TEXT NOT NULL,
    finished_at          TEXT,
    deployment_checksum  TEXT NOT NULL,
    tier                 TEXT NOT NULL   -- 'userspace' | 'privileged'
);

CREATE TABLE IF NOT EXISTS findings (
    finding_id           INTEGER PRIMARY KEY,
    path                 TEXT NOT NULL,
    check_type           TEXT NOT NULL DEFAULT 'file_drift',
    classification       TEXT NOT NULL,  -- only 'anomalous' rows are stored
    detail               TEXT,
    first_seen_scan_id   INTEGER NOT NULL REFERENCES scan_runs(scan_id),
    last_seen_scan_id    INTEGER NOT NULL REFERENCES scan_runs(scan_id),
    resolved_at_scan_id  INTEGER REFERENCES scan_runs(scan_id)
);

CREATE INDEX IF NOT EXISTS idx_findings_open
    ON findings(path) WHERE resolved_at_scan_id IS NULL;

-- Cross-scan state for Secure Boot / kernel-lockdown transition detection.
-- Not part of the baseline schema; specific to the privileged tier's
-- state-transition (not absolute-state) flagging (privileged spec §2).
CREATE TABLE IF NOT EXISTS state_snapshots (
    kind         TEXT PRIMARY KEY,   -- 'secure_boot' | 'lockdown'
    value        TEXT NOT NULL,
    recorded_at  TEXT NOT NULL
);
"#;

pub const TIER_PRIVILEGED: &str = "privileged";

/// A finding as reconciled against prior scans, tagged with its diff status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    New,
    StillOutstanding,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledFinding {
    pub path: String,
    pub status: DiffStatus,
    pub detail: String,
}

/// The three buckets the user-facing report is built from (baseline §4).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub new: Vec<ReconciledFinding>,
    pub still_outstanding: Vec<ReconciledFinding>,
    pub resolved: Vec<ReconciledFinding>,
}

pub struct ScanStore {
    conn: Mutex<Connection>,
}

impl ScanStore {
    pub fn open(path: &Path) -> Result<Self> {
        Self::initialize(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| ScanError::Poisoned)
    }

    /// Open a new scan run, returning its id.
    pub fn start_scan(&self, deployment_checksum: &str, tier: &str) -> Result<i64> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO scan_runs (started_at, deployment_checksum, tier) VALUES (?1, ?2, ?3)",
            params![Utc::now().to_rfc3339(), deployment_checksum, tier],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_scan(&self, scan_id: i64) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE scan_runs SET finished_at = ?1 WHERE scan_id = ?2",
            params![Utc::now().to_rfc3339(), scan_id],
        )?;
        Ok(())
    }

    /// Reconcile this scan's anomalous findings against the prior open set,
    /// implementing the baseline spec's new/still-outstanding/resolved diff
    /// (§4). Non-anomalous findings are ignored here (not persisted).
    ///
    /// Only prior open findings whose `check_type` is in `completed` may be
    /// resolved: a skipped or failed check says nothing about its earlier
    /// findings, so they stay open (a row with an unknown `check_type` is
    /// never resolved either).
    pub fn reconcile(
        &self,
        scan_id: i64,
        findings: &[Finding],
        completed: &[CheckType],
    ) -> Result<ReconcileReport> {
        let anomalies: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.classification == Classification::Anomalous)
            .collect();

        let conn = self.lock()?;
        let tx = conn.unchecked_transaction()?;

        // Prior still-open findings, keyed by path, with their check type.
        let mut prior_open: HashMap<String, Option<CheckType>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT path, check_type FROM findings WHERE resolved_at_scan_id IS NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (path, check_type) = row?;
                prior_open.insert(path, CheckType::from_db(&check_type));
            }
        }

        let mut report = ReconcileReport::default();
        let current_paths: std::collections::HashSet<&str> =
            anomalies.iter().map(|f| f.path.as_str()).collect();

        for finding in &anomalies {
            if prior_open.contains_key(&finding.path) {
                // Still outstanding — bump last_seen.
                tx.execute(
                    "UPDATE findings SET last_seen_scan_id = ?1, detail = ?2 \
                     WHERE path = ?3 AND resolved_at_scan_id IS NULL",
                    params![scan_id, finding.detail, finding.path],
                )?;
                report.still_outstanding.push(ReconciledFinding {
                    path: finding.path.clone(),
                    status: DiffStatus::StillOutstanding,
                    detail: finding.detail.clone(),
                });
            } else {
                // New anomaly.
                tx.execute(
                    "INSERT INTO findings \
                     (path, check_type, classification, detail, first_seen_scan_id, last_seen_scan_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![
                        finding.path,
                        finding.check_type.as_db(),
                        finding.classification.as_db(),
                        finding.detail,
                        scan_id
                    ],
                )?;
                report.new.push(ReconciledFinding {
                    path: finding.path.clone(),
                    status: DiffStatus::New,
                    detail: finding.detail.clone(),
                });
            }
        }

        // Prior open findings of a completed check, absent now → resolved.
        for (path, check_type) in &prior_open {
            let completed_check = check_type.is_some_and(|c| completed.contains(&c));
            if completed_check && !current_paths.contains(path.as_str()) {
                tx.execute(
                    "UPDATE findings SET resolved_at_scan_id = ?1 \
                     WHERE path = ?2 AND resolved_at_scan_id IS NULL",
                    params![scan_id, path],
                )?;
                report.resolved.push(ReconciledFinding {
                    path: path.clone(),
                    status: DiffStatus::Resolved,
                    detail: String::new(),
                });
            }
        }

        tx.commit()?;
        Ok(report)
    }

    /// Read the last recorded snapshot value for a state kind (Secure Boot /
    /// lockdown), or `None` if never recorded.
    pub fn get_state_snapshot(&self, kind: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        let value = conn
            .query_row(
                "SELECT value FROM state_snapshots WHERE kind = ?1",
                params![kind],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }

    /// Persist the current snapshot value for a state kind.
    pub fn set_state_snapshot(&self, kind: &str, value: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO state_snapshots (kind, value, recorded_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(kind) DO UPDATE SET value = excluded.value, recorded_at = excluded.recorded_at",
            params![kind, value, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALL: [CheckType; 5] = [
        CheckType::FileDrift,
        CheckType::KargsDrift,
        CheckType::ModuleAnomaly,
        CheckType::LockdownState,
        CheckType::RootkitSignature,
    ];

    fn anomaly(path: &str) -> Finding {
        Finding::anomalous(CheckType::ModuleAnomaly, path, "detail")
    }

    #[test]
    fn first_scan_reports_all_new() {
        let store = ScanStore::open_in_memory().unwrap();
        let scan = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let report = store
            .reconcile(scan, &[anomaly("module:a"), anomaly("module:b")], &ALL)
            .unwrap();
        assert_eq!(report.new.len(), 2);
        assert!(report.still_outstanding.is_empty());
        assert!(report.resolved.is_empty());
    }

    #[test]
    fn persisting_finding_still_outstanding_next_scan() {
        let store = ScanStore::open_in_memory().unwrap();
        let s1 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        store.reconcile(s1, &[anomaly("module:a")], &ALL).unwrap();

        let s2 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let report = store.reconcile(s2, &[anomaly("module:a")], &ALL).unwrap();
        assert!(report.new.is_empty());
        assert_eq!(report.still_outstanding.len(), 1);
        assert!(report.resolved.is_empty());
    }

    #[test]
    fn finding_gone_next_scan_is_resolved() {
        let store = ScanStore::open_in_memory().unwrap();
        let s1 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        store.reconcile(s1, &[anomaly("module:a")], &ALL).unwrap();

        let s2 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let report = store.reconcile(s2, &[], &ALL).unwrap();
        assert_eq!(report.resolved.len(), 1);
        assert_eq!(report.resolved[0].path, "module:a");

        // A resolved finding does not re-resolve on the following scan.
        let s3 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let report3 = store.reconcile(s3, &[], &ALL).unwrap();
        assert!(report3.resolved.is_empty());
    }

    #[test]
    fn reappearing_finding_is_new_again_after_resolution() {
        let store = ScanStore::open_in_memory().unwrap();
        let s1 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        store.reconcile(s1, &[anomaly("module:a")], &ALL).unwrap();
        let s2 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        store.reconcile(s2, &[], &ALL).unwrap(); // resolved
        let s3 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let report = store.reconcile(s3, &[anomaly("module:a")], &ALL).unwrap();
        assert_eq!(report.new.len(), 1);
    }

    fn typed(check: CheckType, path: &str) -> Finding {
        Finding::anomalous(check, path, "detail")
    }

    #[test]
    fn skipped_checks_never_resolve_their_findings() {
        let store = ScanStore::open_in_memory().unwrap();
        let s1 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let both = [
            typed(CheckType::FileDrift, "/usr/bin/x"),
            typed(CheckType::RootkitSignature, "rootkit:chkrootkit:lkm"),
        ];
        assert_eq!(store.reconcile(s1, &both, &ALL).unwrap().new.len(), 2);

        // Scan 2: both checks skipped (nothing completed, nothing found).
        let s2 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let r2 = store.reconcile(s2, &[], &[]).unwrap();
        assert!(r2.resolved.is_empty(), "skipped checks resolved: {r2:?}");

        // Scan 2b: only file_drift completed -> only it may resolve.
        let s2b = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let r2b = store.reconcile(s2b, &[], &[CheckType::FileDrift]).unwrap();
        assert_eq!(r2b.resolved.len(), 1);
        assert_eq!(r2b.resolved[0].path, "/usr/bin/x");

        // Scan 3: both completed and clean -> the rest resolves.
        let s3 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let r3 = store.reconcile(s3, &[], &ALL).unwrap();
        assert_eq!(r3.resolved.len(), 1);
        assert_eq!(r3.resolved[0].path, "rootkit:chkrootkit:lkm");
    }

    #[test]
    fn a_skipped_check_still_bumps_findings_it_did_report() {
        let store = ScanStore::open_in_memory().unwrap();
        let s1 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let f = [typed(CheckType::FileDrift, "/usr/bin/x")];
        store.reconcile(s1, &f, &ALL).unwrap();
        let s2 = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let r = store.reconcile(s2, &f, &[]).unwrap();
        assert_eq!(r.still_outstanding.len(), 1);
        assert!(r.new.is_empty());
    }

    #[test]
    fn informational_findings_are_not_persisted() {
        let store = ScanStore::open_in_memory().unwrap();
        let scan = store.start_scan("dep1", TIER_PRIVILEGED).unwrap();
        let info = Finding::informational(CheckType::LockdownState, "lockdown:kernel", "none");
        let report = store.reconcile(scan, &[info], &ALL).unwrap();
        assert!(report.new.is_empty());
    }

    #[test]
    fn state_snapshot_roundtrip() {
        let store = ScanStore::open_in_memory().unwrap();
        assert_eq!(store.get_state_snapshot("secure_boot").unwrap(), None);
        store.set_state_snapshot("secure_boot", "enabled").unwrap();
        assert_eq!(
            store.get_state_snapshot("secure_boot").unwrap().as_deref(),
            Some("enabled")
        );
        store.set_state_snapshot("secure_boot", "disabled").unwrap();
        assert_eq!(
            store.get_state_snapshot("secure_boot").unwrap().as_deref(),
            Some("disabled")
        );
    }
}
