//! Whether this bridge manages the recommended background-service rules
//! (prompt-slot D), and where it keeps the user's choices:
//! `<state>/curated-defaults.json`, under the same [`Storage`] that
//! [`resolve_storage`](crate::resolve_storage) resolved.
//!
//! Like blocklists, only the system bridge with a persistent state directory
//! installs them. The per-user bridge's daemon connection is the legacy TCP
//! one, where another program could pose as the firewall service and its
//! `OK` would make an entry read "Installed". Without saved settings a rule
//! the user deleted couldn't be remembered, so it could come back after a
//! restart. Either way the page lists the entries and says why nothing is
//! installed.

use snitchwatch_bridge::curated::manager::CuratedDefaults;
use snitchwatch_bridge::curated::store::FILE_NAME;

use crate::storage::{BridgeMode, Storage};

/// Shown on the per-user bridge.
pub const PER_USER_REASON: &str = "Recommended rules need the system-wide Snitchwatch \
     service; this per-user service can't be sure it is talking to the real firewall service.";

/// Shown without a usable state directory.
pub const NO_STORAGE_REASON: &str = "Recommended rules need saved settings, so that a rule you \
     delete stays deleted, and Snitchwatch has no usable state folder.";

/// Points `curated` at `storage` for `mode`.
pub(crate) fn configure(curated: &CuratedDefaults, storage: &Storage, mode: BridgeMode) {
    curated.set_storage(storage.status());
    match storage {
        Storage::Persistent(dir) if mode == BridgeMode::System => {
            curated.attach_file(dir.join(FILE_NAME));
        }
        Storage::Persistent(_) => curated.set_unavailable(PER_USER_REASON),
        Storage::Ephemeral(_) => curated.set_unavailable(NO_STORAGE_REASON),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::EphemeralReason;
    use crate::test_daemon::daemon;
    use snitchwatch_bridge::ws_messages::ServerMessage;

    fn curated() -> CuratedDefaults {
        let daemon = daemon(Vec::new());
        CuratedDefaults::new(daemon.commands, daemon.cache, daemon.broadcast)
    }

    fn state_of(curated: &CuratedDefaults) -> (bool, Option<String>) {
        match curated.message() {
            ServerMessage::SetCuratedDefaults {
                storage,
                unavailable,
                ..
            } => (storage.persistent, unavailable),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_the_system_bridge_with_saved_settings_manages_them() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().canonicalize().unwrap();
        let system = curated();
        configure(
            &system,
            &Storage::Persistent(state.clone()),
            BridgeMode::System,
        );
        assert_eq!(state_of(&system), (true, None));

        let per_user = curated();
        configure(&per_user, &Storage::Persistent(state), BridgeMode::User);
        assert_eq!(state_of(&per_user).1.as_deref(), Some(PER_USER_REASON));

        for mode in [BridgeMode::System, BridgeMode::User] {
            let ephemeral = curated();
            let storage = Storage::Ephemeral(EphemeralReason::Unusable("bad folder".into()));
            configure(&ephemeral, &storage, mode);
            assert_eq!(
                state_of(&ephemeral),
                (false, Some(NO_STORAGE_REASON.to_string()))
            );
        }
    }
}
