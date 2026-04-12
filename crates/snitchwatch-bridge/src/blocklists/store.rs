//! SQLite storage for blocklist subscriptions and their resolved entries.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    pub url: String,
    pub display_name: String,
    pub format_hint: Option<String>,
    pub refresh_interval_secs: i64,
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub last_fetch_status: FetchStatus,
    pub entry_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchStatus {
    Pending,
    Ok,
    Failed { reason: String },
}

impl FetchStatus {
    fn to_db(&self) -> (&'static str, Option<&str>) {
        match self {
            FetchStatus::Pending => ("pending", None),
            FetchStatus::Ok => ("ok", None),
            FetchStatus::Failed { reason } => ("failed", Some(reason.as_str())),
        }
    }

    fn from_db(kind: &str, reason: Option<String>) -> Self {
        match kind {
            "ok" => FetchStatus::Ok,
            "failed" => FetchStatus::Failed {
                reason: reason.unwrap_or_default(),
            },
            _ => FetchStatus::Pending,
        }
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store mutex poisoned")]
    Poisoned,
}

pub struct BlocklistStore {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS subscriptions (
    id                   TEXT PRIMARY KEY,
    url                  TEXT NOT NULL,
    display_name         TEXT NOT NULL,
    format_hint          TEXT,
    refresh_interval_secs INTEGER NOT NULL,
    last_fetched_at      TEXT,
    last_fetch_status    TEXT NOT NULL,
    last_fetch_reason    TEXT,
    entry_count          INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS entries (
    subscription_id TEXT NOT NULL,
    host            TEXT NOT NULL,
    PRIMARY KEY (subscription_id, host),
    FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_entries_sub ON entries(subscription_id);
"#;

impl BlocklistStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::initialize(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::initialize(conn)
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

    pub fn upsert_subscription(&self, sub: &Subscription) -> Result<(), StoreError> {
        let conn = self.lock()?;
        let (kind, reason) = sub.last_fetch_status.to_db();
        conn.execute(
            r#"
            INSERT INTO subscriptions
                (id, url, display_name, format_hint, refresh_interval_secs,
                 last_fetched_at, last_fetch_status, last_fetch_reason, entry_count)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                url                   = excluded.url,
                display_name          = excluded.display_name,
                format_hint           = excluded.format_hint,
                refresh_interval_secs = excluded.refresh_interval_secs,
                last_fetched_at       = excluded.last_fetched_at,
                last_fetch_status     = excluded.last_fetch_status,
                last_fetch_reason     = excluded.last_fetch_reason,
                entry_count           = excluded.entry_count
            "#,
            params![
                sub.id,
                sub.url,
                sub.display_name,
                sub.format_hint,
                sub.refresh_interval_secs,
                sub.last_fetched_at.map(|t| t.to_rfc3339()),
                kind,
                reason,
                sub.entry_count,
            ],
        )?;
        Ok(())
    }

    pub fn get_subscription(&self, id: &str) -> Result<Option<Subscription>, StoreError> {
        let conn = self.lock()?;
        conn.query_row(
            r#"
            SELECT id, url, display_name, format_hint, refresh_interval_secs,
                   last_fetched_at, last_fetch_status, last_fetch_reason, entry_count
            FROM subscriptions WHERE id = ?1
            "#,
            params![id],
            row_to_subscription,
        )
        .optional()
        .map_err(StoreError::from)
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, url, display_name, format_hint, refresh_interval_secs,
                   last_fetched_at, last_fetch_status, last_fetch_reason, entry_count
            FROM subscriptions ORDER BY id
            "#,
        )?;
        let rows = stmt
            .query_map([], row_to_subscription)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_subscription(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM subscriptions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn replace_entries(&self, sub_id: &str, hosts: &[&str]) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM entries WHERE subscription_id = ?1",
            params![sub_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO entries (subscription_id, host) VALUES (?1, ?2)",
            )?;
            for host in hosts {
                stmt.execute(params![sub_id, host])?;
            }
        }
        tx.execute(
            "UPDATE subscriptions SET entry_count = ?1 WHERE id = ?2",
            params![hosts.len() as i64, sub_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_entries(&self, sub_id: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT host FROM entries WHERE subscription_id = ?1 ORDER BY host",
        )?;
        let rows = stmt
            .query_map(params![sub_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subscription> {
    let id: String = row.get(0)?;
    let url: String = row.get(1)?;
    let display_name: String = row.get(2)?;
    let format_hint: Option<String> = row.get(3)?;
    let refresh_interval_secs: i64 = row.get(4)?;
    let last_fetched_at: Option<String> = row.get(5)?;
    let kind: String = row.get(6)?;
    let reason: Option<String> = row.get(7)?;
    let entry_count: i64 = row.get(8)?;
    let last_fetched_at = last_fetched_at
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|t| t.with_timezone(&Utc)));
    Ok(Subscription {
        id,
        url,
        display_name,
        format_hint,
        refresh_interval_secs,
        last_fetched_at,
        last_fetch_status: FetchStatus::from_db(&kind, reason),
        entry_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> BlocklistStore {
        BlocklistStore::open_in_memory().expect("in-memory store opens")
    }

    #[test]
    fn fresh_store_has_zero_subscriptions() {
        let store = open_in_memory();
        let all = store.list_subscriptions().expect("list");
        assert!(all.is_empty(), "fresh store must be empty, got {all:?}");
    }

    #[test]
    fn upsert_subscription_round_trips() {
        let store = open_in_memory();
        let sub = Subscription {
            id: "stevenblack".to_string(),
            url: "https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts".to_string(),
            display_name: "StevenBlack".to_string(),
            format_hint: Some("hosts".to_string()),
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).expect("upsert");
        let loaded = store.get_subscription("stevenblack").expect("get").expect("found");
        assert_eq!(loaded, sub);
    }

    #[test]
    fn delete_subscription_cascades_entries() {
        let store = open_in_memory();
        let sub = Subscription {
            id: "test".to_string(),
            url: "https://example.invalid/list.txt".to_string(),
            display_name: "Test".to_string(),
            format_hint: None,
            refresh_interval_secs: 3600,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).unwrap();
        store
            .replace_entries("test", &["doubleclick.net", "google-analytics.com"])
            .unwrap();
        store.delete_subscription("test").unwrap();
        assert_eq!(store.list_subscriptions().unwrap().len(), 0);
        assert_eq!(store.list_entries("test").unwrap().len(), 0);
    }
}
