//! SQLite storage for firewall profiles (Little-Snitch-parity "At Home" /
//! "Public Wi-Fi" / "Office" style switchable profiles).
//!
//! A profile is a named set of network matchers (globs matched against the
//! active NetworkManager connection id / SSID) plus a small list of rule
//! overrides meant to be materialized into opensnitchd while the profile is
//! active (issue #46 Part 2), plus the user's manual choice of profile and
//! the network it was made on (issue #82). Unlike
//! [`crate::blocklists::store::BlocklistStore`] (which splits
//! subscriptions and their many fetched entries across two tables), a
//! profile's matcher list and rule list are both small, user-authored
//! collections, so they're stored as JSON text columns on a single row rather
//! than normalized into child tables — simpler, and there is no independent
//! "refresh" process that would want to bulk-replace just one of them.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One rule owned by a profile, installed while the profile is active (see
/// [`crate::profiles::materializer`], issue #46 Part 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRule {
    /// Stable id within the profile (used for add/remove), independent of the
    /// materialized opensnitchd rule name.
    pub id: String,
    /// `"allow"`, `"deny"` or `"reject"`.
    pub action: String,
    /// Part 1's single condition: an operand such as `"dest.host"`. Empty
    /// when `operator` is set.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operand: String,
    /// Part 1's value to match against the operand.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// The rule editor's conditions, in #48's wire shape (Part 2). A rule
    /// saved by Part 1 has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<serde_json::Value>,
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

impl From<crate::sqlite_file::OpenError> for StoreError {
    fn from(error: crate::sqlite_file::OpenError) -> Self {
        match error {
            crate::sqlite_file::OpenError::Sqlite(e) => Self::Sqlite(e),
            other => Self::Io(other.into_io()),
        }
    }
}

pub struct ProfileStore {
    conn: Mutex<Connection>,
}

/// `PRAGMA user_version` of [`SCHEMA`]. Version 2 (issue #82) adds the
/// manual choice table; profile rules may also carry the editor's
/// conditions, which a version 1 bridge can't read.
const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS profiles (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    network_matchers  TEXT NOT NULL,
    rules             TEXT NOT NULL,
    active            INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS manual_choice (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    profile_id  TEXT,
    network     TEXT
);
"#;

/// A profile the user chose by hand (`None`: none), and the network
/// connection it was chosen on (`None`: none known). Issue #82: it holds
/// while the bridge observes that same network, across restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualChoice {
    pub profile_id: Option<String>,
    pub network: Option<String>,
}

impl ProfileStore {
    /// Open (or create) the database at `path`, owner-only (0600), exactly as
    /// [`crate::blocklists::store::BlocklistStore::open`] does, through
    /// [`crate::sqlite_file::open_owner_only`].
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = crate::sqlite_file::open_owner_only(path)?;
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
        crate::sqlite_file::require_known_schema(&conn, SCHEMA)?;
        conn.execute_batch(SCHEMA)?;
        // Only when it changes: rewriting the same value still writes to the
        // file, and a store that turns out to be unreadable is left as it is.
        if version != SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        }
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

    /// The saved manual choice, if any.
    pub fn manual_choice(&self) -> Result<Option<ManualChoice>, StoreError> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT profile_id, network FROM manual_choice WHERE id = 1",
                [],
                |row| {
                    Ok(ManualChoice {
                        profile_id: row.get(0)?,
                        network: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_manual_choice(&self, choice: &ManualChoice) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO manual_choice (id, profile_id, network) VALUES (1, ?1, ?2) \
             ON CONFLICT(id) DO UPDATE SET profile_id = excluded.profile_id, \
             network = excluded.network",
            params![choice.profile_id, choice.network],
        )?;
        Ok(())
    }

    pub fn clear_manual_choice(&self) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM manual_choice", [])?;
        Ok(())
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
            operator: None,
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
            operator: None,
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
        assert!(crate::sqlite_file::open_connection(&path).is_err());
        assert!(crate::sqlite_file::open_connection(&target).is_ok());
    }

    /// Opening a store that is already at the current version must not write
    /// to it: an unreadable store is left as it is, and `open` is the first
    /// thing that touches it.
    #[test]
    fn reopening_a_current_store_leaves_the_file_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        let store = ProfileStore::open(&path).unwrap();
        store.upsert_profile(&profile("home", &["Home"])).unwrap();
        drop(store);
        let before = std::fs::read(&path).unwrap();
        drop(ProfileStore::open(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);
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

    /// Issue #82: a manual choice is saved with the network it was made on,
    /// and survives a reopen.
    #[test]
    fn a_manual_choice_survives_a_reopen_and_can_be_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        let store = ProfileStore::open(&path).unwrap();
        assert_eq!(store.manual_choice().unwrap(), None);
        let choice = ManualChoice {
            profile_id: Some("office".into()),
            network: Some("Home-5G".into()),
        };
        store.set_manual_choice(&choice).unwrap();
        drop(store);
        let reopened = ProfileStore::open(&path).unwrap();
        assert_eq!(reopened.manual_choice().unwrap(), Some(choice));
        let none = ManualChoice {
            profile_id: None,
            network: None,
        };
        reopened.set_manual_choice(&none).unwrap();
        assert_eq!(reopened.manual_choice().unwrap(), Some(none));
        reopened.clear_manual_choice().unwrap();
        assert_eq!(reopened.manual_choice().unwrap(), None);
    }

    /// A store Part 1 wrote (version 1, no manual-choice table) opens, keeps
    /// its profiles, and is moved to version 2.
    #[test]
    fn a_version_1_store_is_upgraded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        let v1 = Connection::open(&path).unwrap();
        v1.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS profiles (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    network_matchers  TEXT NOT NULL,
    rules             TEXT NOT NULL,
    active            INTEGER NOT NULL DEFAULT 0
);
INSERT INTO profiles VALUES ('home', 'Home', '[]',
    '[{"id":"r1","action":"deny","operand":"dest.host","data":"x.example"}]', 1);
PRAGMA user_version = 1;
"#,
        )
        .unwrap();
        drop(v1);
        let store = ProfileStore::open(&path).expect("a v1 store opens");
        let home = store.get_active().unwrap().unwrap();
        assert_eq!(home.rules[0].operand, "dest.host");
        assert_eq!(home.rules[0].operator, None);
        assert_eq!(store.manual_choice().unwrap(), None);
        let version: i64 = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
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
