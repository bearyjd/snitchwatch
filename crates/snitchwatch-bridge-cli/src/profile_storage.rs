//! Where the bridge keeps profiles and the active-profile choice (issue #46
//! Part 1): `<state>/profiles.sqlite3`, next to the blocklist store, under the
//! same [`Storage`] that [`resolve_storage`](crate::resolve_storage) resolved
//! (with its directory, mode and ownership checks). Owner-only (0600) and
//! symlink-refusing, as `ProfileStore::open` mirrors `BlocklistStore::open`.
//!
//! The profile store's storage is tracked apart from the blocklist store's:
//! either can fall back to memory while the other persists, and each GUI
//! page is told about its own. Any bridge with a `Persistent` state
//! directory saves profiles, the per-user one too. Only the system bridge
//! applies the active profile's rules to the firewall (issue #46 Part 2);
//! the per-user one installs nothing and says why on the Profiles page. A
//! system bridge whose saved profiles can't be read (its store fell back to
//! memory) changes no profile rule at all: with the keep-set unknown, a
//! purge would delete the active profile's rules, the user's denies among
//! them (the #45 PR B lesson). It installs nothing, deletes nothing, and
//! the page says so. A store with one unreadable row counts as unreadable
//! as a whole: skipping the row could skip the active profile.
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
use snitchwatch_bridge::profiles::{
    DaemonProfileSink, NoopProfileRuleSink, ProfileRuleSink, ProfilesManager,
};
use snitchwatch_bridge::ws_messages::ClientMessage;
use tracing::{error, info};

use crate::storage::{BridgeMode, DaemonRules, EphemeralReason, Storage};

/// Why a system bridge whose saved profiles can't be read changes none.
pub const UNREADABLE_REASON: &str = "Snitchwatch can't read its saved profiles (profiles.sqlite3 \
     in its state folder), so it changes no profile rules: rules it installed earlier were left \
     in place. Fix or move that file, then restart Snitchwatch's background service.";

/// Why a system bridge with nowhere to save profiles changes none.
pub const NOWHERE_REASON: &str = "Snitchwatch has no place to save profiles here, so it changes \
     no profile rules: rules it installed earlier were left in place.";

/// Why a per-user bridge applies no profile rules.
pub const PER_USER_REASON: &str = "Profiles are applied to the firewall only by the \
     system-wide Snitchwatch service; in this per-user setup another program could pose as the \
     firewall service.";

/// The profile database's file name inside the state directory.
pub const PROFILE_DB_FILE: &str = "profiles.sqlite3";

/// The bridge's profile manager: persisted in `<state>/profiles.sqlite3`
/// when its store opened `Persistent` (in memory otherwise), and applying
/// the active profile's rules through `daemon` only for the system bridge
/// with a store it could read.
pub(crate) fn build_profiles_manager(
    storage: Storage,
    mode: BridgeMode,
    daemon: DaemonRules,
) -> Result<Arc<ProfilesManager>> {
    let (store, storage) = open_profile_store(storage)?;
    let sink: Arc<dyn ProfileRuleSink> = match (mode, &storage) {
        (BridgeMode::System, Storage::Persistent(_)) => {
            Arc::new(DaemonProfileSink::new(daemon.commands, daemon.rules))
        }
        (BridgeMode::System, Storage::Ephemeral(EphemeralReason::Unusable(_))) => {
            Arc::new(NoopProfileRuleSink::new(UNREADABLE_REASON))
        }
        (BridgeMode::System, Storage::Ephemeral(_)) => {
            Arc::new(NoopProfileRuleSink::new(NOWHERE_REASON))
        }
        (BridgeMode::User, _) => Arc::new(NoopProfileRuleSink::new(PER_USER_REASON)),
    };
    let manager = ProfilesManager::new(store)
        .with_storage_status(storage.status())
        .with_rule_sink(sink);
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

    fn daemon() -> DaemonRules {
        use snitchwatch_bridge::cache::rules::RulesSync;
        use snitchwatch_bridge::daemon_commands::{DaemonCommands, DaemonTransport};
        let rules = RulesSync::new(tokio::sync::broadcast::channel(4).0);
        DaemonRules {
            commands: DaemonCommands::new(DaemonTransport::Unix, rules.clone()),
            rules: rules.cache(),
        }
    }

    fn build(storage: Storage, mode: BridgeMode) -> Arc<ProfilesManager> {
        build_profiles_manager(storage, mode, daemon()).unwrap()
    }

    /// Issue #46 Part 2: only the system bridge applies profile rules, also
    /// from a store in memory (whose first pass removes an earlier run's
    /// rules); the per-user one says why it applies none.
    #[test]
    fn only_the_system_bridge_applies_profiles() {
        let (_dir, state) = state();
        let system = build(Storage::Persistent(state.clone()), BridgeMode::System);
        assert_eq!(system.not_applied_reason(), None);
        let user = build(Storage::Persistent(state), BridgeMode::User);
        assert_eq!(user.not_applied_reason().as_deref(), Some(PER_USER_REASON));
    }

    /// PR #104 review HIGH: a system bridge whose saved profiles can't be
    /// read must not purge: with the keep-set unknown it installs nothing
    /// and deletes nothing, and says why (the #45 PR B lesson).
    #[test]
    fn a_system_bridge_that_cant_read_its_profiles_changes_no_profile_rule() {
        let memory = build(
            Storage::Ephemeral(EphemeralReason::InProcess),
            BridgeMode::System,
        );
        assert_eq!(memory.not_applied_reason().as_deref(), Some(NOWHERE_REASON));
        // One bad row makes the whole store unreadable (the safe choice:
        // a skipped row could be the active profile's).
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
        let unreadable = build(Storage::Persistent(state), BridgeMode::System);
        assert!(!unreadable.storage_status().persistent);
        assert_eq!(
            unreadable.not_applied_reason().as_deref(),
            Some(UNREADABLE_REASON)
        );
        assert!(UNREADABLE_REASON.contains("left in place"));
    }

    #[test]
    fn the_manager_reports_the_resolved_storage() {
        let (_dir, state) = state();
        let persistent = build(Storage::Persistent(state.clone()), BridgeMode::User);
        assert!(persistent.storage_status().persistent);
        assert_eq!(persistent.storage_status().reason, None);

        std::fs::remove_file(state.join(PROFILE_DB_FILE)).unwrap();
        std::fs::create_dir(state.join(PROFILE_DB_FILE)).unwrap();
        let fallback = build(Storage::Persistent(state), BridgeMode::User);
        assert!(!fallback.storage_status().persistent);
        assert!(fallback
            .storage_status()
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("profile store: ")));

        let in_process = build(
            Storage::Ephemeral(EphemeralReason::InProcess),
            BridgeMode::User,
        );
        assert!(!in_process.storage_status().persistent);
        assert_eq!(in_process.storage_status().reason, None);
    }
}
