//! `scans.db` — the userspace tier's scan history and findings, plus the
//! new/still-outstanding/resolved reconcile of baseline design §4.
//!
//! This store also holds the **userspace per-surface baseline** table
//! (`userspace_baseline`). The spec's `baseline.db` is the privileged tier's
//! full-`/usr` hash cache; the userspace surfaces this crate actually checks
//! (shell rc files, systemd `--user` units, autostart entries, Flatpak
//! overrides, user-reachable listeners) are all user-writable and need no
//! privilege to snapshot. Per baseline design stop-and-ask §2 ("should the
//! userspace tier be allowed to build its own baseline cache
//! opportunistically when it can"), we take the yes-for-non-privileged-paths
//! branch and keep that trust-on-first-use baseline in the userspace-owned
//! `scans.db`, never in the privileged store.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use super::StoreError;
use crate::finding::{Anomaly, FindingStatus, ReportedFinding};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS scan_runs (
    scan_id             INTEGER PRIMARY KEY,
    started_at          TEXT NOT NULL,
    finished_at         TEXT,
    deployment_checksum TEXT NOT NULL,
    tier                TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS findings (
    finding_id          INTEGER PRIMARY KEY,
    path                TEXT NOT NULL,
    check_name          TEXT NOT NULL,
    classification      TEXT NOT NULL,
    detail              TEXT,
    first_seen_scan_id  INTEGER NOT NULL REFERENCES scan_runs(scan_id),
    last_seen_scan_id   INTEGER NOT NULL REFERENCES scan_runs(scan_id),
    resolved_at_scan_id INTEGER REFERENCES scan_runs(scan_id)
);

-- At most one *open* (unresolved) finding per path.
CREATE UNIQUE INDEX IF NOT EXISTS idx_findings_open_path
    ON findings(path) WHERE resolved_at_scan_id IS NULL;

CREATE TABLE IF NOT EXISTS userspace_baseline (
    surface        TEXT NOT NULL,
    item_key       TEXT NOT NULL,
    recorded_value TEXT NOT NULL,
    recorded_at    TEXT NOT NULL,
    PRIMARY KEY (surface, item_key)
);
"#;

/// Outcome of one scan's reconcile — the three buckets shown to the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconcile {
    pub new: Vec<ReportedFinding>,
    pub still_outstanding: Vec<ReportedFinding>,
    pub resolved: Vec<ReportedFinding>,
}

/// Handle to `scans.db`.
pub struct ScanStore {
    conn: Mutex<Connection>,
}

impl ScanStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::initialize(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    /// Open a new scan run, returning its id.
    pub fn begin_scan(
        &self,
        deployment_checksum: &str,
        tier: &str,
        started_at: DateTime<Utc>,
    ) -> Result<i64, StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO scan_runs (started_at, deployment_checksum, tier) VALUES (?1, ?2, ?3)",
            params![started_at.to_rfc3339(), deployment_checksum, tier],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Stamp a scan run finished.
    pub fn finish_scan(&self, scan_id: i64, finished_at: DateTime<Utc>) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE scan_runs SET finished_at = ?1 WHERE scan_id = ?2",
            params![finished_at.to_rfc3339(), scan_id],
        )?;
        Ok(())
    }

    /// Reconcile this scan's anomaly set against the prior still-open
    /// findings, implementing baseline design §4's diff exactly. All writes
    /// happen in one transaction so a partial scan never leaves the findings
    /// table half-updated.
    ///
    /// `anomalies` is deduplicated by `path` (path is the finding identity);
    /// if two checks report the same path the later one's detail wins.
    pub fn reconcile(&self, scan_id: i64, anomalies: &[Anomaly]) -> Result<Reconcile, StoreError> {
        // Dedupe current anomalies by path.
        let mut current: HashMap<&str, &Anomaly> = HashMap::new();
        for a in anomalies {
            current.insert(a.path.as_str(), a);
        }

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        // Snapshot prior open findings (resolved_at_scan_id IS NULL). These
        // are open as of *before* this scan, since we haven't written this
        // scan's rows yet.
        let prior_open: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT finding_id, path FROM findings WHERE resolved_at_scan_id IS NULL",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let prior_by_path: HashMap<String, i64> =
            prior_open.iter().map(|(id, p)| (p.clone(), *id)).collect();

        let mut result = Reconcile::default();

        // New + still-outstanding.
        for (path, anomaly) in &current {
            if let Some(&finding_id) = prior_by_path.get(*path) {
                tx.execute(
                    "UPDATE findings SET last_seen_scan_id = ?1, detail = ?2, check_name = ?3 \
                     WHERE finding_id = ?4",
                    params![scan_id, anomaly.detail, anomaly.check, finding_id],
                )?;
                result.still_outstanding.push(ReportedFinding {
                    finding_id,
                    path: anomaly.path.clone(),
                    detail: anomaly.detail.clone(),
                    status: FindingStatus::StillOutstanding,
                });
            } else {
                tx.execute(
                    "INSERT INTO findings \
                        (path, check_name, classification, detail, \
                         first_seen_scan_id, last_seen_scan_id, resolved_at_scan_id) \
                     VALUES (?1, ?2, 'anomalous', ?3, ?4, ?4, NULL)",
                    params![anomaly.path, anomaly.check, anomaly.detail, scan_id],
                )?;
                result.new.push(ReportedFinding {
                    finding_id: tx.last_insert_rowid(),
                    path: anomaly.path.clone(),
                    detail: anomaly.detail.clone(),
                    status: FindingStatus::New,
                });
            }
        }

        // Resolved: prior open findings not present now.
        for (finding_id, path) in &prior_open {
            if !current.contains_key(path.as_str()) {
                tx.execute(
                    "UPDATE findings SET resolved_at_scan_id = ?1 WHERE finding_id = ?2",
                    params![scan_id, finding_id],
                )?;
                let detail: Option<String> = tx.query_row(
                    "SELECT detail FROM findings WHERE finding_id = ?1",
                    params![finding_id],
                    |row| row.get(0),
                )?;
                result.resolved.push(ReportedFinding {
                    finding_id: *finding_id,
                    path: path.clone(),
                    detail: detail.unwrap_or_default(),
                    status: FindingStatus::Resolved,
                });
            }
        }

        tx.commit()?;

        result.new.sort_by(|a, b| a.path.cmp(&b.path));
        result.still_outstanding.sort_by(|a, b| a.path.cmp(&b.path));
        result.resolved.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(result)
    }

    // ---- userspace per-surface baseline (trust-on-first-use) ------------

    /// Whether a surface has ever had its baseline captured.
    pub fn surface_baseline_is_empty(&self, surface: &str) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM userspace_baseline WHERE surface = ?1",
            params![surface],
            |row| row.get(0),
        )?;
        Ok(count == 0)
    }

    /// Load a surface's baseline as `item_key -> recorded_value`.
    pub fn load_surface_baseline(
        &self,
        surface: &str,
    ) -> Result<HashMap<String, String>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT item_key, recorded_value FROM userspace_baseline WHERE surface = ?1",
        )?;
        let rows = stmt
            .query_map(params![surface], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Capture (or overwrite) a surface's baseline items. Used on first-run
    /// trust-on-first-use and by an explicit "accept current state" action.
    pub fn capture_surface_baseline(
        &self,
        surface: &str,
        items: &[(String, String)],
        recorded_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO userspace_baseline (surface, item_key, recorded_value, recorded_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(surface, item_key) DO UPDATE SET \
                    recorded_value = excluded.recorded_value, \
                    recorded_at    = excluded.recorded_at",
            )?;
            let ts = recorded_at.to_rfc3339();
            for (key, value) in items {
                stmt.execute(params![surface, key, value, ts])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ScanStore {
        ScanStore::open_in_memory().unwrap()
    }

    fn anomaly(path: &str, detail: &str) -> Anomaly {
        Anomaly::new("test-check", path, detail)
    }

    #[test]
    fn first_scan_all_anomalies_are_new() {
        let s = store();
        let scan = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        let r = s
            .reconcile(scan, &[anomaly("/a", "one"), anomaly("/b", "two")])
            .unwrap();
        assert_eq!(r.new.len(), 2);
        assert!(r.still_outstanding.is_empty());
        assert!(r.resolved.is_empty());
        assert_eq!(r.new[0].path, "/a");
        assert_eq!(r.new[0].status, FindingStatus::New);
    }

    #[test]
    fn persisting_anomaly_becomes_still_outstanding() {
        let s = store();
        let s1 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        s.reconcile(s1, &[anomaly("/a", "one")]).unwrap();
        let s2 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        let r = s.reconcile(s2, &[anomaly("/a", "one-still")]).unwrap();
        assert!(r.new.is_empty());
        assert_eq!(r.still_outstanding.len(), 1);
        assert_eq!(r.still_outstanding[0].path, "/a");
        // Detail is refreshed to the latest observation.
        assert_eq!(r.still_outstanding[0].detail, "one-still");
    }

    #[test]
    fn disappearing_anomaly_becomes_resolved() {
        let s = store();
        let s1 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        s.reconcile(s1, &[anomaly("/a", "one"), anomaly("/b", "two")])
            .unwrap();
        let s2 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        let r = s.reconcile(s2, &[anomaly("/a", "one")]).unwrap();
        assert_eq!(r.still_outstanding.len(), 1);
        assert_eq!(r.resolved.len(), 1);
        assert_eq!(r.resolved[0].path, "/b");
        assert_eq!(r.resolved[0].status, FindingStatus::Resolved);
    }

    #[test]
    fn resolved_then_reappearing_is_new_again() {
        let s = store();
        let s1 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        s.reconcile(s1, &[anomaly("/a", "one")]).unwrap();
        // Scan 2: /a gone -> resolved.
        let s2 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        let r2 = s.reconcile(s2, &[]).unwrap();
        assert_eq!(r2.resolved.len(), 1);
        // Scan 3: /a back -> new (a fresh row; the old one stays resolved).
        let s3 = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        let r3 = s.reconcile(s3, &[anomaly("/a", "again")]).unwrap();
        assert_eq!(r3.new.len(), 1);
        assert!(r3.still_outstanding.is_empty());
    }

    #[test]
    fn duplicate_paths_dedupe_to_one_finding() {
        let s = store();
        let scan = s.begin_scan("dep0", "userspace", Utc::now()).unwrap();
        let r = s
            .reconcile(scan, &[anomaly("/a", "first"), anomaly("/a", "second")])
            .unwrap();
        assert_eq!(r.new.len(), 1);
    }

    #[test]
    fn surface_baseline_capture_and_load() {
        let s = store();
        assert!(s.surface_baseline_is_empty("shell-rc").unwrap());
        s.capture_surface_baseline(
            "shell-rc",
            &[("/home/u/.bashrc".to_string(), "hash1".to_string())],
            Utc::now(),
        )
        .unwrap();
        assert!(!s.surface_baseline_is_empty("shell-rc").unwrap());
        let loaded = s.load_surface_baseline("shell-rc").unwrap();
        assert_eq!(
            loaded.get("/home/u/.bashrc").map(String::as_str),
            Some("hash1")
        );
    }
}
