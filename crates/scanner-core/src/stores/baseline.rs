//! `baseline.db` — the privileged tier's full-hash baseline cache
//! (baseline design §4), keyed by deployment checksum.
//!
//! Phase 5 (userspace) does not *build* this cache — that full-content
//! hashing is the privileged tier's job (Phase 6). This module provides the
//! schema and read/write API now so both tiers share one definition, and so
//! the userspace tier can consult a cached baseline entry (its recorded
//! checksum/mode/owner) as a cheap per-file signal without re-hashing.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::StoreError;

/// A row of `deployments` — one cached baseline per unique deployment
/// checksum the system has booted into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentBaseline {
    pub deployment_checksum: String,
    pub base_checksum: String,
    /// Snapshot of `rpm-ostree status` packages[], stored verbatim as JSON.
    pub layered_packages_json: String,
    pub computed_at: DateTime<Utc>,
}

/// A single expected-state entry for a path under a deployment's baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineEntry {
    pub path: String,
    /// `'base_tree'` | `'layered_pkg:<name>'` | `'dynamic_allowlist'`.
    pub expected_source: String,
    /// `None` for `dynamic_allowlist` entries (checksum irrelevant there).
    pub expected_checksum: Option<String>,
    pub expected_mode: Option<i64>,
    pub expected_owner: Option<String>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS deployments (
    deployment_checksum   TEXT PRIMARY KEY,
    base_checksum         TEXT NOT NULL,
    layered_packages_json TEXT NOT NULL,
    computed_at           TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS baseline_entries (
    deployment_checksum TEXT NOT NULL REFERENCES deployments(deployment_checksum) ON DELETE CASCADE,
    path                TEXT NOT NULL,
    expected_source     TEXT NOT NULL,
    expected_checksum   TEXT,
    expected_mode       INTEGER,
    expected_owner      TEXT,
    PRIMARY KEY (deployment_checksum, path)
);

CREATE INDEX IF NOT EXISTS idx_baseline_entries_dep
    ON baseline_entries(deployment_checksum);
"#;

/// Handle to `baseline.db`.
pub struct BaselineStore {
    conn: Mutex<Connection>,
}

impl BaselineStore {
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

    /// True when a cached baseline already exists for this deployment
    /// checksum — the invalidation key of baseline design §2. The privileged
    /// tier recomputes only when this is `false`.
    pub fn has_baseline_for(&self, deployment_checksum: &str) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM deployments WHERE deployment_checksum = ?1",
                params![deployment_checksum],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Insert/replace a deployment's baseline header and entries atomically.
    pub fn put_baseline(
        &self,
        header: &DeploymentBaseline,
        entries: &[BaselineEntry],
    ) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO deployments
                (deployment_checksum, base_checksum, layered_packages_json, computed_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(deployment_checksum) DO UPDATE SET
                base_checksum         = excluded.base_checksum,
                layered_packages_json = excluded.layered_packages_json,
                computed_at           = excluded.computed_at
            "#,
            params![
                header.deployment_checksum,
                header.base_checksum,
                header.layered_packages_json,
                header.computed_at.to_rfc3339(),
            ],
        )?;
        tx.execute(
            "DELETE FROM baseline_entries WHERE deployment_checksum = ?1",
            params![header.deployment_checksum],
        )?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO baseline_entries
                    (deployment_checksum, path, expected_source,
                     expected_checksum, expected_mode, expected_owner)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )?;
            for e in entries {
                stmt.execute(params![
                    header.deployment_checksum,
                    e.path,
                    e.expected_source,
                    e.expected_checksum,
                    e.expected_mode,
                    e.expected_owner,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Look up a single baseline entry (cheap per-file signal for the
    /// userspace tier).
    pub fn get_entry(
        &self,
        deployment_checksum: &str,
        path: &str,
    ) -> Result<Option<BaselineEntry>, StoreError> {
        let conn = self.lock()?;
        conn.query_row(
            r#"
            SELECT path, expected_source, expected_checksum, expected_mode, expected_owner
            FROM baseline_entries
            WHERE deployment_checksum = ?1 AND path = ?2
            "#,
            params![deployment_checksum, path],
            |row| {
                Ok(BaselineEntry {
                    path: row.get(0)?,
                    expected_source: row.get(1)?,
                    expected_checksum: row.get(2)?,
                    expected_mode: row.get(3)?,
                    expected_owner: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> BaselineStore {
        BaselineStore::open_in_memory().unwrap()
    }

    fn header(checksum: &str) -> DeploymentBaseline {
        DeploymentBaseline {
            deployment_checksum: checksum.to_string(),
            base_checksum: "base0".to_string(),
            layered_packages_json: "[]".to_string(),
            computed_at: Utc::now(),
        }
    }

    #[test]
    fn no_baseline_initially() {
        let s = store();
        assert!(!s.has_baseline_for("dep0").unwrap());
    }

    #[test]
    fn put_then_has_and_get() {
        let s = store();
        let entries = vec![BaselineEntry {
            path: "/usr/bin/opensnitchd".to_string(),
            expected_source: "layered_pkg:opensnitch".to_string(),
            expected_checksum: Some("deadbeef".to_string()),
            expected_mode: Some(0o755),
            expected_owner: Some("root".to_string()),
        }];
        s.put_baseline(&header("dep0"), &entries).unwrap();
        assert!(s.has_baseline_for("dep0").unwrap());
        let got = s
            .get_entry("dep0", "/usr/bin/opensnitchd")
            .unwrap()
            .unwrap();
        assert_eq!(got.expected_checksum.as_deref(), Some("deadbeef"));
        assert_eq!(got.expected_source, "layered_pkg:opensnitch");
    }

    #[test]
    fn put_baseline_replaces_entries() {
        let s = store();
        s.put_baseline(
            &header("dep0"),
            &[BaselineEntry {
                path: "/usr/bin/a".to_string(),
                expected_source: "base_tree".to_string(),
                expected_checksum: Some("aaa".to_string()),
                expected_mode: None,
                expected_owner: None,
            }],
        )
        .unwrap();
        // Re-put with a different entry set.
        s.put_baseline(
            &header("dep0"),
            &[BaselineEntry {
                path: "/usr/bin/b".to_string(),
                expected_source: "base_tree".to_string(),
                expected_checksum: Some("bbb".to_string()),
                expected_mode: None,
                expected_owner: None,
            }],
        )
        .unwrap();
        assert!(s.get_entry("dep0", "/usr/bin/a").unwrap().is_none());
        assert!(s.get_entry("dep0", "/usr/bin/b").unwrap().is_some());
    }
}
