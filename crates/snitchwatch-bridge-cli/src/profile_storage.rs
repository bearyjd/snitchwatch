//! Where the bridge keeps profiles and the active-profile choice (issue #46
//! Part 1): `<state>/profiles.sqlite3`, next to the blocklist store, under the
//! same [`Storage`] that [`resolve_storage`](crate::resolve_storage) resolved
//! (with its directory, mode and ownership checks). Owner-only (0600) and
//! symlink-refusing, as `ProfileStore::open` mirrors `BlocklistStore::open`.
//!
//! The profile store's storage is tracked apart from the blocklist store's:
//! either can fall back to memory while the other persists, and each GUI
//! page is told about its own. Any bridge with a `Persistent` state
//! directory saves profiles, the per-user one too: profiles install no
//! firewall rule (the sink is `NoopProfileRuleSink` until Part 2), so unlike
//! blocklists there is nothing to gate on [`BridgeMode`](crate::BridgeMode).
//!
//! A store that can't be opened, or opens but can't be read, falls back to
//! memory as `Unusable("profile store: …")`, logged at `error!` and shown on
//! the Profiles page; the file is left as it is. (An unreadable blocklist
//! store is kept instead, so that its installed rules are never purged;
//! profiles have no installed rules, and `SetProfiles` reads the store
//! directly, so an unreadable one would hide every profile.)

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use snitchwatch_bridge::profiles::store::{ProfileStore, StoreError};
use snitchwatch_bridge::profiles::ProfilesManager;
use tracing::{error, info};

use crate::storage::{EphemeralReason, Storage};

/// The profile database's file name inside the state directory.
pub const PROFILE_DB_FILE: &str = "profiles.sqlite3";

/// The bridge's profile manager: persisted in `<state>/profiles.sqlite3`
/// when its store opened `Persistent` (in memory otherwise), with the
/// default no-op rule sink.
pub(crate) fn build_profiles_manager(storage: Storage) -> Result<Arc<ProfilesManager>> {
    let (store, storage) = open_profile_store(storage)?;
    let manager = ProfilesManager::new(store).with_storage_status(storage.status());
    Ok(Arc::new(manager))
}

/// Open the store `storage` calls for. A persistent store that fails to open
/// or read falls back to memory as `Unusable("profile store: …")`; only a
/// failing in-memory open is fatal.
fn open_profile_store(storage: Storage) -> Result<(Arc<ProfileStore>, Storage)> {
    let storage = match storage {
        Storage::Persistent(dir) => {
            let path = dir.join(PROFILE_DB_FILE);
            match open_readable(&path) {
                Ok(store) => {
                    info!(path = %path.display(), "opened persistent profile store");
                    return Ok((Arc::new(store), Storage::Persistent(dir)));
                }
                Err(e) => {
                    let reason = format!("profile store: {e}");
                    error!(path = %path.display(), %reason, "profiles are kept in memory only");
                    Storage::Ephemeral(EphemeralReason::Unusable(reason))
                }
            }
        }
        ephemeral => ephemeral,
    };
    let store = ProfileStore::open_in_memory().context("failed to open in-memory profile store")?;
    Ok((Arc::new(store), storage))
}

/// Open the store and read every profile once.
fn open_readable(path: &Path) -> std::result::Result<ProfileStore, StoreError> {
    let store = ProfileStore::open(path)?;
    store.list_profiles()?;
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::profiles::store::Profile;

    fn unusable_reason(storage: &Storage) -> &str {
        match storage {
            Storage::Ephemeral(EphemeralReason::Unusable(reason)) => reason,
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    fn state() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().canonicalize().unwrap();
        (dir, state)
    }

    fn home() -> Profile {
        Profile {
            id: "home".into(),
            name: "At Home".into(),
            network_matchers: vec![],
            rules: vec![],
            active: false,
        }
    }

    #[test]
    fn a_persistent_store_is_written_in_the_state_directory() {
        let (_dir, state) = state();
        let (store, storage) = open_profile_store(Storage::Persistent(state.clone())).unwrap();
        assert_eq!(storage, Storage::Persistent(state.clone()));
        store.upsert_profile(&home()).unwrap();
        let reopened = ProfileStore::open(&state.join(PROFILE_DB_FILE)).unwrap();
        assert_eq!(reopened.list_profiles().unwrap(), vec![home()]);
    }

    /// Without a usable state directory nothing is written: the store is in
    /// memory and the reason passes through unchanged.
    #[test]
    fn ephemeral_storage_stays_in_memory() {
        for reason in [
            EphemeralReason::InProcess,
            EphemeralReason::NotConfigured,
            EphemeralReason::Unusable("unexpected state directory /x".into()),
        ] {
            let (store, storage) = open_profile_store(Storage::Ephemeral(reason.clone())).unwrap();
            assert_eq!(storage, Storage::Ephemeral(reason));
            store.upsert_profile(&home()).unwrap();
        }
    }

    /// A store that can't be opened falls back to memory and says why.
    #[test]
    fn an_unopenable_store_falls_back_to_memory() {
        let (_dir, state) = state();
        std::fs::create_dir(state.join(PROFILE_DB_FILE)).unwrap();
        let (store, storage) = open_profile_store(Storage::Persistent(state)).unwrap();
        assert!(unusable_reason(&storage).starts_with("profile store: "));
        assert!(store.list_profiles().unwrap().is_empty());
    }

    /// A store that opens but can't be read falls back to memory too, and
    /// the saved file is left as it was.
    #[test]
    fn an_unreadable_store_falls_back_to_memory_and_is_left_alone() {
        let (_dir, state) = state();
        let path = state.join(PROFILE_DB_FILE);
        ProfileStore::open(&path)
            .unwrap()
            .upsert_profile(&home())
            .unwrap();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("UPDATE profiles SET rules = 'not json';")
            .unwrap();
        let (store, storage) = open_profile_store(Storage::Persistent(state)).unwrap();
        assert!(unusable_reason(&storage).starts_with("profile store: "));
        assert!(store.list_profiles().unwrap().is_empty());
        let rules: String = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT rules FROM profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rules, "not json", "the saved profiles were changed");
    }

    #[test]
    fn the_manager_reports_the_resolved_storage() {
        let (_dir, state) = state();
        let persistent = build_profiles_manager(Storage::Persistent(state.clone())).unwrap();
        assert!(persistent.storage_status().persistent);
        assert_eq!(persistent.storage_status().reason, None);

        std::fs::remove_file(state.join(PROFILE_DB_FILE)).unwrap();
        std::fs::create_dir(state.join(PROFILE_DB_FILE)).unwrap();
        let fallback = build_profiles_manager(Storage::Persistent(state)).unwrap();
        assert!(!fallback.storage_status().persistent);
        assert!(fallback
            .storage_status()
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("profile store: ")));

        let in_process =
            build_profiles_manager(Storage::Ephemeral(EphemeralReason::InProcess)).unwrap();
        assert!(!in_process.storage_status().persistent);
        assert_eq!(in_process.storage_status().reason, None);
    }
}
