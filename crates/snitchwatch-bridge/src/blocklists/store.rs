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
    pub last_attempt_at: Option<DateTime<Utc>>,
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
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("store mutex poisoned")]
    Poisoned,
    #[error("database schema version {0} is newer than this Snitchwatch supports")]
    NewerSchema(i64),
}

pub struct BlocklistStore {
    conn: Mutex<Connection>,
}

/// `PRAGMA user_version` of [`SCHEMA`].
const SCHEMA_VERSION: i64 = 1;

/// `entries` is `WITHOUT ROWID`: its composite primary key is the table, so
/// each host is stored once and the key already indexes `subscription_id`.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS subscriptions (
    id                   TEXT PRIMARY KEY,
    url                  TEXT NOT NULL,
    display_name         TEXT NOT NULL,
    format_hint          TEXT,
    refresh_interval_secs INTEGER NOT NULL,
    last_fetched_at      TEXT,
    last_attempt_at      TEXT,
    last_fetch_status    TEXT NOT NULL,
    last_fetch_reason    TEXT,
    entry_count          INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS entries (
    subscription_id TEXT NOT NULL,
    host            TEXT NOT NULL,
    PRIMARY KEY (subscription_id, host),
    FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
) WITHOUT ROWID;
"#;

const SUBSCRIPTION_COLUMNS: &str = "id, url, display_name, format_hint, refresh_interval_secs, \
     last_fetched_at, last_attempt_at, last_fetch_status, last_fetch_reason, entry_count";

impl BlocklistStore {
    /// Open (or create) the database at `path`, owner-only (0600). The path
    /// is opened with `O_NOFOLLOW` and must be a regular file; the mode is
    /// set through that handle. SQLite gives its journal files the
    /// database's mode.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            )));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        drop(file);
        let conn = Connection::open(path)?;
        Self::initialize(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::initialize(conn)
    }

    fn initialize(conn: Connection) -> Result<Self, StoreError> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema(version));
        }
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    /// Hold the connection lock, as a long write does (tests only).
    #[cfg(test)]
    pub(crate) fn lock_for_test(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.lock().unwrap()
    }

    pub fn upsert_subscription(&self, sub: &Subscription) -> Result<(), StoreError> {
        let conn = self.lock()?;
        let (kind, reason) = sub.last_fetch_status.to_db();
        conn.execute(
            r#"
            INSERT INTO subscriptions
                (id, url, display_name, format_hint, refresh_interval_secs,
                 last_fetched_at, last_attempt_at, last_fetch_status, last_fetch_reason,
                 entry_count)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                url                   = excluded.url,
                display_name          = excluded.display_name,
                format_hint           = excluded.format_hint,
                refresh_interval_secs = excluded.refresh_interval_secs,
                last_fetched_at       = excluded.last_fetched_at,
                last_attempt_at       = excluded.last_attempt_at,
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
                sub.last_attempt_at.map(|t| t.to_rfc3339()),
                kind,
                reason,
                sub.entry_count,
            ],
        )?;
        Ok(())
    }

    /// Update an existing subscription's row. Returns false (and creates
    /// nothing) if it was removed meanwhile.
    pub fn update_subscription(&self, sub: &Subscription) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        Ok(update_row(&conn, sub)? > 0)
    }

    /// Replace a subscription's entries and update its row in one
    /// transaction. Returns false (and writes nothing) if it was removed.
    pub fn replace_entries_and_update(
        &self,
        sub: &Subscription,
        hosts: &[String],
    ) -> Result<bool, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if update_row(&tx, sub)? == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM entries WHERE subscription_id = ?1",
            params![sub.id],
        )?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO entries (subscription_id, host) VALUES (?1, ?2)")?;
            for host in hosts {
                stmt.execute(params![sub.id, host])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn get_subscription(&self, id: &str) -> Result<Option<Subscription>, StoreError> {
        let conn = self.lock()?;
        conn.query_row(
            &format!("SELECT {SUBSCRIPTION_COLUMNS} FROM subscriptions WHERE id = ?1"),
            params![id],
            row_to_subscription,
        )
        .optional()
        .map_err(StoreError::from)
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUBSCRIPTION_COLUMNS} FROM subscriptions ORDER BY id"
        ))?;
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
            let mut stmt =
                tx.prepare("INSERT INTO entries (subscription_id, host) VALUES (?1, ?2)")?;
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
        let mut stmt =
            conn.prepare("SELECT host FROM entries WHERE subscription_id = ?1 ORDER BY host")?;
        let rows = stmt
            .query_map(params![sub_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// At most `limit` of a subscription's hosts, in order, from `offset`.
    pub fn list_entries_page(
        &self,
        sub_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<String>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT host FROM entries WHERE subscription_id = ?1 ORDER BY host LIMIT ?2 OFFSET ?3",
        )?;
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let rows = stmt
            .query_map(params![sub_id, limit, offset], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn update_row(conn: &Connection, sub: &Subscription) -> rusqlite::Result<usize> {
    let (kind, reason) = sub.last_fetch_status.to_db();
    conn.execute(
        r#"
        UPDATE subscriptions SET
            url = ?2, display_name = ?3, format_hint = ?4, refresh_interval_secs = ?5,
            last_fetched_at = ?6, last_attempt_at = ?7, last_fetch_status = ?8,
            last_fetch_reason = ?9, entry_count = ?10
        WHERE id = ?1
        "#,
        params![
            sub.id,
            sub.url,
            sub.display_name,
            sub.format_hint,
            sub.refresh_interval_secs,
            sub.last_fetched_at.map(|t| t.to_rfc3339()),
            sub.last_attempt_at.map(|t| t.to_rfc3339()),
            kind,
            reason,
            sub.entry_count,
        ],
    )
}

fn parse_time(value: Option<String>) -> Option<DateTime<Utc>> {
    value.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    })
}

fn row_to_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subscription> {
    let kind: String = row.get(7)?;
    let reason: Option<String> = row.get(8)?;
    Ok(Subscription {
        id: row.get(0)?,
        url: row.get(1)?,
        display_name: row.get(2)?,
        format_hint: row.get(3)?,
        refresh_interval_secs: row.get(4)?,
        last_fetched_at: parse_time(row.get(5)?),
        last_attempt_at: parse_time(row.get(6)?),
        last_fetch_status: FetchStatus::from_db(&kind, reason),
        entry_count: row.get(9)?,
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
            last_attempt_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        store.upsert_subscription(&sub).expect("upsert");
        let loaded = store
            .get_subscription("stevenblack")
            .expect("get")
            .expect("found");
        assert_eq!(loaded, sub);
    }

    /// The database names every subscribed URL; in system mode it sits in
    /// `/var/lib/snitchwatch`. Created owner-only, and tightened if it exists.
    #[test]
    fn open_creates_the_database_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocklists.sqlite3");
        let store = BlocklistStore::open(&path).expect("open");
        store
            .upsert_subscription(&Subscription {
                id: "a".into(),
                url: "https://x.example/a".into(),
                display_name: "a".into(),
                format_hint: None,
                refresh_interval_secs: 1,
                last_fetched_at: None,
                last_attempt_at: None,
                last_fetch_status: FetchStatus::Pending,
                entry_count: 0,
            })
            .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        drop(store);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let reopened = BlocklistStore::open(&path).expect("reopen");
        assert_eq!(mode(&path), 0o600, "an existing database is tightened");
        assert_eq!(reopened.list_subscriptions().unwrap().len(), 1);
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
            last_attempt_at: None,
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

    fn sub(id: &str) -> Subscription {
        Subscription {
            id: id.into(),
            url: format!("https://x.example/{id}"),
            display_name: id.into(),
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_attempt_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        }
    }

    /// Issue #45 (S3): the composite primary key already indexes
    /// `subscription_id`; a rowid table plus a second index stored every
    /// host three times.
    #[test]
    fn entries_are_stored_once_without_rowid_and_versioned() {
        let store = open_in_memory();
        let conn = store.lock().unwrap();
        let entries_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'entries'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(entries_sql.contains("WITHOUT ROWID"), "{entries_sql}");
        let indexes: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 0, "no extra index on entries");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocklists.sqlite3");
        drop(BlocklistStore::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        assert!(BlocklistStore::open(&path).is_err());
    }

    /// S7: the database path is opened without following a symlink, and the
    /// link's target is left alone.
    #[test]
    fn open_refuses_a_symlinked_database() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.path().join("blocklists.sqlite3");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(BlocklistStore::open(&path).is_err());
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the symlink target was modified");
    }

    #[test]
    fn last_attempt_round_trips() {
        let store = open_in_memory();
        let mut s = sub("a");
        s.last_attempt_at = Some(
            DateTime::parse_from_rfc3339("2026-10-08T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        store.upsert_subscription(&s).unwrap();
        assert_eq!(store.get_subscription("a").unwrap().unwrap(), s);
    }

    /// A refresh never resurrects a subscription removed while it ran.
    #[test]
    fn updates_never_recreate_a_removed_subscription() {
        let store = open_in_memory();
        assert!(!store.update_subscription(&sub("gone")).unwrap());
        assert!(!store
            .replace_entries_and_update(&sub("gone"), &["a.example".to_string()])
            .unwrap());
        assert!(store.list_subscriptions().unwrap().is_empty());
        assert!(store.list_entries("gone").unwrap().is_empty());

        store.upsert_subscription(&sub("here")).unwrap();
        let mut updated = sub("here");
        updated.entry_count = 2;
        assert!(store
            .replace_entries_and_update(
                &updated,
                &["b.example".to_string(), "a.example".to_string()]
            )
            .unwrap());
        assert_eq!(
            store.get_subscription("here").unwrap().unwrap().entry_count,
            2
        );
        assert_eq!(
            store.list_entries("here").unwrap(),
            vec!["a.example", "b.example"]
        );
    }

    #[test]
    fn entries_are_read_a_page_at_a_time() {
        let store = open_in_memory();
        store.upsert_subscription(&sub("p")).unwrap();
        let hosts: Vec<String> = (0..5).map(|i| format!("h{i}.example")).collect();
        let mut updated = sub("p");
        updated.entry_count = 5;
        store.replace_entries_and_update(&updated, &hosts).unwrap();
        assert_eq!(
            store.list_entries_page("p", 0, 2).unwrap(),
            vec!["h0.example", "h1.example"]
        );
        assert_eq!(
            store.list_entries_page("p", 4, 2).unwrap(),
            vec!["h4.example"]
        );
        assert!(store.list_entries_page("p", 9, 2).unwrap().is_empty());
    }
}
