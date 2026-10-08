//! SQLite storage for firewall profiles (Little-Snitch-parity "At Home" /
//! "Public Wi-Fi" / "Office" style switchable profiles).
//!
//! A profile is a named set of network matchers (globs matched against the
//! active NetworkManager connection id / SSID) plus a small list of rule
//! overrides meant to be materialized into opensnitchd while the profile is
//! active. Nothing installs them yet: the bridge's production sink is
//! [`crate::profiles::NoopProfileRuleSink`] (issue #46 Part 2). Unlike [`crate::blocklists::store::BlocklistStore`] (which splits
//! subscriptions and their many fetched entries across two tables), a
//! profile's matcher list and rule list are both small, user-authored
//! collections, so they're stored as JSON text columns on a single row rather
//! than normalized into child tables — simpler, and there is no independent
//! "refresh" process that would want to bulk-replace just one of them.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One rule override owned by a profile, materialized as a `deny`/`allow`
/// opensnitchd rule while the owning profile is active (see
/// [`crate::profiles::materializer`]) and handed to the manager's rule sink,
/// which in production installs nothing yet (issue #46 Part 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRule {
    /// Stable id within the profile (used for add/remove), independent of the
    /// materialized opensnitchd rule name.
    pub id: String,
    /// `"allow"` or `"deny"`.
    pub action: String,
    /// Operand, e.g. `"dest.host"` or `"process.path"`.
    pub operand: String,
    /// Value to match against the operand.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// Glob patterns (see `translator::glob`) matched against the active
    /// NetworkManager connection id or SSID to auto-activate this profile.
    pub network_matchers: Vec<String>,
    pub rules: Vec<ProfileRule>,
    /// At most one profile is active at a time; enforced by
    /// [`ProfileStore::set_active`], not a DB constraint.
    pub active: bool,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store mutex poisoned")]
    Poisoned,
    #[error("invalid stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown profile id: {0}")]
    UnknownProfile(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("database schema version {0} is newer than this Snitchwatch supports")]
    NewerSchema(i64),
}

pub struct ProfileStore {
    conn: Mutex<Connection>,
}

/// `PRAGMA user_version` of [`SCHEMA`].
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS profiles (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    network_matchers  TEXT NOT NULL,
    rules             TEXT NOT NULL,
    active            INTEGER NOT NULL DEFAULT 0
);
"#;

impl ProfileStore {
    /// Open (or create) the database at `path`, owner-only (0600), exactly as
    /// [`crate::blocklists::store::BlocklistStore::open`] does: the path is
    /// opened with `O_NOFOLLOW` and must be a regular file; the mode is set
    /// through that handle. SQLite gives its journal files the database's
    /// mode.
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
        let conn = open_connection(path)?;
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
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    pub fn upsert_profile(&self, profile: &Profile) -> Result<(), StoreError> {
        let conn = self.lock()?;
        let matchers = serde_json::to_string(&profile.network_matchers)?;
        let rules = serde_json::to_string(&profile.rules)?;
        conn.execute(
            r#"
            INSERT INTO profiles (id, name, network_matchers, rules, active)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(id) DO UPDATE SET
                name              = excluded.name,
                network_matchers  = excluded.network_matchers,
                rules             = excluded.rules,
                active            = excluded.active
            "#,
            params![
                profile.id,
                profile.name,
                matchers,
                rules,
                profile.active as i64,
            ],
        )?;
        Ok(())
    }

    pub fn get_profile(&self, id: &str) -> Result<Option<Profile>, StoreError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, network_matchers, rules, active FROM profiles WHERE id = ?1",
            params![id],
            row_to_profile,
        )
        .optional()?
        .transpose()
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, network_matchers, rules, active FROM profiles ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], row_to_profile)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM profiles WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Get the currently active profile, if any. Exactly zero or one row can
    /// ever have `active = 1` (enforced by [`Self::set_active`]).
    pub fn get_active(&self) -> Result<Option<Profile>, StoreError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, name, network_matchers, rules, active FROM profiles WHERE active = 1",
            [],
            row_to_profile,
        )
        .optional()?
        .transpose()
    }

    /// Mark `id` as the sole active profile, clearing `active` on every other
    /// row in the same transaction. Passing `None` clears every profile's
    /// active flag (the "no profile active" / default state).
    pub fn set_active(&self, id: Option<&str>) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute("UPDATE profiles SET active = 0", [])?;
        if let Some(id) = id {
            let updated =
                tx.execute("UPDATE profiles SET active = 1 WHERE id = ?1", params![id])?;
            if updated == 0 {
                // sqlite doesn't distinguish "matched zero rows" from
                // "succeeded" on its own, and silently no-op-ing an unknown
                // profile id would leave every profile inactive with no
                // signal to the caller — surface it as a real error instead.
                return Err(StoreError::UnknownProfile(id.to_string()));
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// `Connection::open`'s flags without `URI`, plus `NOFOLLOW`: SQLite opens
/// the path again after [`ProfileStore::open`]'s checks.
fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
}

fn row_to_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Profile, StoreError>> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let matchers_json: String = row.get(2)?;
    let rules_json: String = row.get(3)?;
    let active: i64 = row.get(4)?;

    let parsed = (|| -> Result<Profile, StoreError> {
        let network_matchers: Vec<String> = serde_json::from_str(&matchers_json)?;
        let rules: Vec<ProfileRule> = serde_json::from_str(&rules_json)?;
        Ok(Profile {
            id,
            name,
            network_matchers,
            rules,
            active: active != 0,
        })
    })();
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> ProfileStore {
        ProfileStore::open_in_memory().expect("in-memory store opens")
    }

    fn profile(id: &str, matchers: &[&str]) -> Profile {
        Profile {
            id: id.to_string(),
            name: id.to_string(),
            network_matchers: matchers.iter().map(|s| s.to_string()).collect(),
            rules: vec![],
            active: false,
        }
    }

    #[test]
    fn fresh_store_has_zero_profiles() {
        let store = open_in_memory();
        assert!(store.list_profiles().unwrap().is_empty());
    }

    #[test]
    fn upsert_profile_round_trips() {
        let store = open_in_memory();
        let mut p = profile("home", &["Home-WiFi", "home-*"]);
        p.rules.push(ProfileRule {
            id: "r1".into(),
            action: "allow".into(),
            operand: "dest.host".into(),
            data: "nas.local".into(),
        });
        store.upsert_profile(&p).unwrap();
        let loaded = store.get_profile("home").unwrap().unwrap();
        assert_eq!(loaded, p);
    }

    #[test]
    fn delete_profile_removes_row() {
        let store = open_in_memory();
        store.upsert_profile(&profile("temp", &[])).unwrap();
        store.delete_profile("temp").unwrap();
        assert!(store.get_profile("temp").unwrap().is_none());
    }

    #[test]
    fn set_active_enforces_single_active_profile() {
        let store = open_in_memory();
        store.upsert_profile(&profile("home", &["Home"])).unwrap();
        store
            .upsert_profile(&profile("office", &["Office"]))
            .unwrap();

        store.set_active(Some("home")).unwrap();
        assert_eq!(store.get_active().unwrap().unwrap().id, "home");

        store.set_active(Some("office")).unwrap();
        let active = store.get_active().unwrap().unwrap();
        assert_eq!(active.id, "office");
        assert!(!store.get_profile("home").unwrap().unwrap().active);
    }

    #[test]
    fn set_active_none_clears_active_profile() {
        let store = open_in_memory();
        store.upsert_profile(&profile("home", &["Home"])).unwrap();
        store.set_active(Some("home")).unwrap();
        store.set_active(None).unwrap();
        assert!(store.get_active().unwrap().is_none());
    }

    #[test]
    fn set_active_unknown_id_errors() {
        let store = open_in_memory();
        assert!(store.set_active(Some("nope")).is_err());
    }

    /// Issue #46 Part 1: profiles and the active-profile choice survive a
    /// reopen of the same database file.
    #[test]
    fn a_reopened_store_keeps_profiles_and_the_active_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        let store = ProfileStore::open(&path).expect("open");
        let mut home = profile("home", &["Home*"]);
        home.rules.push(ProfileRule {
            id: "r1".into(),
            action: "deny".into(),
            operand: "dest.host".into(),
            data: "ads.example".into(),
        });
        store.upsert_profile(&home).unwrap();
        store.upsert_profile(&profile("office", &[])).unwrap();
        store.set_active(Some("home")).unwrap();
        drop(store);

        let reopened = ProfileStore::open(&path).expect("reopen");
        let profiles = reopened.list_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        let active = reopened.get_active().unwrap().expect("an active profile");
        assert_eq!(active.id, "home");
        assert_eq!(active.rules, home.rules);
        assert!(!reopened.get_profile("office").unwrap().unwrap().active);
    }

    /// Profile names and network matchers (Wi-Fi names) are nobody else's
    /// business; in system mode the file sits in `/var/lib/snitchwatch`.
    /// Created owner-only, and tightened if it exists (as
    /// `BlocklistStore::open`).
    #[test]
    fn open_creates_the_database_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        let store = ProfileStore::open(&path).expect("open");
        store.upsert_profile(&profile("home", &["Home"])).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        drop(store);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let reopened = ProfileStore::open(&path).expect("reopen");
        assert_eq!(mode(&path), 0o600, "an existing database is tightened");
        assert_eq!(reopened.list_profiles().unwrap().len(), 1);
    }

    /// The database path is opened without following a symlink, and the
    /// link's target is left alone.
    #[test]
    fn open_refuses_a_symlinked_database() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.path().join("profiles.sqlite3");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(ProfileStore::open(&path).is_err());
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the symlink target was modified");
    }

    /// SQLite re-opens the path itself after the `O_NOFOLLOW` check, so it
    /// must refuse a symlink too (a swap in between).
    #[test]
    fn sqlite_itself_refuses_a_symlinked_database() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.sqlite3");
        drop(Connection::open(&target).unwrap());
        let path = dir.path().join("profiles.sqlite3");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(open_connection(&path).is_err());
        assert!(open_connection(&target).is_ok());
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        drop(ProfileStore::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        assert!(matches!(
            ProfileStore::open(&path),
            Err(StoreError::NewerSchema(99))
        ));
    }

    #[test]
    fn list_profiles_is_ordered_by_id() {
        let store = open_in_memory();
        store.upsert_profile(&profile("b", &[])).unwrap();
        store.upsert_profile(&profile("a", &[])).unwrap();
        let ids: Vec<String> = store
            .list_profiles()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }
}
