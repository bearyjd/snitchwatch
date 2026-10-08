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
//! the Profiles page. Its contents are left as they are: opening only tightens
//! the file's mode to 0600 (and writes `user_version` if the file is new or
//! older), and an unreadable store's reason says so, so the user doesn't
//! recreate profiles that are still there. (An unreadable blocklist store is
//! kept instead, so that its installed rules are never purged; profiles have
//! no installed rules, and `SetProfiles` reads the store directly, so an
//! unreadable one would hide every profile.)

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use snitchwatch_bridge::profiles::store::{ProfileStore, StoreError};
use snitchwatch_bridge::profiles::ProfilesManager;
use snitchwatch_bridge::ws_messages::ClientMessage;
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
                    let reason = e.reason();
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

/// Why the persistent profile store isn't used.
enum OpenFailure {
    /// It couldn't be opened (or was refused).
    Open(StoreError),
    /// It opened but its profiles can't be read.
    Unreadable(StoreError),
}

impl OpenFailure {
    /// The sentence shown on the Profiles page. For a store that opened but
    /// can't be read it says the saved profiles are still on disk, so nobody
    /// recreates them (the next restart would load the old ones again).
    fn reason(&self) -> String {
        match self {
            Self::Open(e) => format!("profile store: {e}"),
            Self::Unreadable(e) => format!(
                "profile store: the saved profiles in {PROFILE_DB_FILE} can't be read ({e}). \
                 They were left as they are, so don't recreate them: fix or move the file and \
                 restart"
            ),
        }
    }
}

/// Open the store and read every profile once.
fn open_readable(path: &Path) -> std::result::Result<ProfileStore, OpenFailure> {
    let store = ProfileStore::open(path).map_err(OpenFailure::Open)?;
    store.list_profiles().map_err(OpenFailure::Unreadable)?;
    Ok(store)
}

/// True for every `ClientMessage` variant `ProfilesManager` owns handling of.
/// Kept as a free function (rather than inlined into the pump's `match`) so
/// it reads as one clear routing decision at the call site.
pub(crate) fn is_profile_message(msg: &ClientMessage) -> bool {
    matches!(
        msg,
        ClientMessage::CreateProfile { .. }
            | ClientMessage::UpdateProfile { .. }
            | ClientMessage::DeleteProfile { .. }
            | ClientMessage::ActivateProfile { .. }
            | ClientMessage::DeactivateProfile
            | ClientMessage::AddProfileRule { .. }
            | ClientMessage::RemoveProfileRule { .. }
    )
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

    /// The page shows this reason with an empty list. It must say the saved
    /// profiles are still there, or the user recreates them and loses them at
    /// the next restart.
    #[test]
    fn an_unreadable_store_says_the_saved_profiles_were_left_as_they_are() {
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
        let (_, storage) = open_profile_store(Storage::Persistent(state)).unwrap();
        let reason = unusable_reason(&storage);
        assert!(reason.starts_with("profile store: "), "{reason}");
        assert!(reason.contains("left as they are"), "{reason}");
        assert!(
            reason.contains("PROFILE_DB_FILE_PLACEHOLDER") || reason.contains(PROFILE_DB_FILE),
            "{reason}"
        );
    }

    /// A store that opens and reads and has nothing to leave alone, or can't
    /// be opened at all, has no saved profiles to promise anything about.
    #[test]
    fn an_unopenable_store_does_not_claim_saved_profiles() {
        let (_dir, state) = state();
        std::fs::create_dir(state.join(PROFILE_DB_FILE)).unwrap();
        let (_, storage) = open_profile_store(Storage::Persistent(state)).unwrap();
        assert!(!unusable_reason(&storage).contains("left as they are"));
    }

    /// Opening the store reads the file but never changes it: the saved
    /// profiles are exactly as they were when the page says so.
    #[test]
    fn an_unreadable_store_file_is_byte_for_byte_what_it_was() {
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
        let before = std::fs::read(&path).unwrap();
        open_profile_store(Storage::Persistent(state)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// Run `f` on its own thread, failing rather than hanging if it never
    /// returns.
    fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(value) => value,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("the open hung"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("the open panicked"),
        }
    }

    /// A FIFO named `<db>-journal` hangs SQLite at startup; the store falls
    /// back to memory with a plain reason instead. SQLite deletes a journal
    /// beside an empty database unread, so the store is saved once first:
    /// that is the case that hangs.
    #[test]
    fn a_fifo_beside_the_profile_database_falls_back_to_memory_without_hanging() {
        use std::os::unix::ffi::OsStrExt;
        let (_dir, state) = state();
        ProfileStore::open(&state.join(PROFILE_DB_FILE))
            .unwrap()
            .upsert_profile(&home())
            .unwrap();
        let journal = state.join(format!("{PROFILE_DB_FILE}-journal"));
        let c_path = std::ffi::CString::new(journal.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let (store, storage) =
            within(move || open_profile_store(Storage::Persistent(state)).unwrap());
        let reason = unusable_reason(&storage);
        assert!(reason.starts_with("profile store: "), "{reason}");
        assert!(
            reason.contains("-journal") && reason.contains("regular file"),
            "{reason}"
        );
        assert!(store.list_profiles().unwrap().is_empty());
    }

    /// A database with a view where the profiles table belongs is refused,
    /// and left as it is.
    #[test]
    fn a_crafted_profile_database_falls_back_to_memory_and_is_left_alone() {
        let (_dir, state) = state();
        let path = state.join(PROFILE_DB_FILE);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE VIEW profiles AS WITH RECURSIVE c(x) AS \
                 (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT x FROM c;",
            )
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let (store, storage) =
            within(move || open_profile_store(Storage::Persistent(state)).unwrap());
        assert!(unusable_reason(&storage).contains("view named profiles"));
        assert!(store.list_profiles().unwrap().is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
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
