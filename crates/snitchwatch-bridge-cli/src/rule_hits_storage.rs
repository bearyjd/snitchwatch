//! Where the per-rule hit counts are kept (P2.6 Part 1, owner decision N1:
//! they survive a bridge restart). `<state>/rule_hits.json`, under the same
//! [`Storage`] that [`resolve_storage`](crate::resolve_storage) resolved
//! (with its directory, mode and ownership checks), for the per-user bridge
//! and the system bridge alike.
//!
//! The file's own format and hardened reads and writes live in
//! `snitchwatch_bridge::cache::rule_hits_file`. Here is only the choice: a
//! `Persistent` directory keeps the counts there; an `Ephemeral` storage keeps
//! them in memory, and the `RuleHits` message tells clients why. A file that
//! can't be read or later can't be written falls back to memory the same way
//! (logged at `error!`/`warn!` and shown), and the bridge keeps running.

use snitchwatch_bridge::cache::rule_hits_handle::RuleHitsHandle;

use crate::storage::Storage;

/// The saved counts' file name inside the state directory.
pub const RULE_HITS_FILE: &str = "rule_hits.json";

/// Points `hits` at `storage`: restores what an earlier run saved, or, with
/// no usable state directory, records why the counts stay in memory.
pub(crate) fn configure(hits: &RuleHitsHandle, storage: &Storage) {
    match storage {
        Storage::Persistent(dir) => hits.attach_file(dir.join(RULE_HITS_FILE)),
        Storage::Ephemeral(_) => hits.set_storage(storage.status()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::EphemeralReason;
    use snitchwatch_bridge::ws_messages::ServerMessage;
    use tokio::sync::broadcast;

    fn storage_of(hits: &RuleHitsHandle) -> (bool, Option<String>) {
        match hits.message() {
            ServerMessage::RuleHits { storage, .. } => (storage.persistent, storage.reason),
            other => panic!("expected RuleHits, got {other:?}"),
        }
    }

    fn handle() -> RuleHitsHandle {
        RuleHitsHandle::new(broadcast::channel(4).0)
    }

    #[test]
    fn a_persistent_directory_keeps_the_counts_in_a_file_there() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().canonicalize().unwrap();
        let hits = handle();
        configure(&hits, &Storage::Persistent(state));
        assert_eq!(storage_of(&hits), (true, None));
    }

    /// Without a usable state directory the counts stay in memory and only
    /// an `Unusable` reason is shown, as for blocklists and profiles.
    #[test]
    fn ephemeral_storage_keeps_the_counts_in_memory_and_says_so() {
        for (reason, shown) in [
            (EphemeralReason::InProcess, None),
            (EphemeralReason::NotConfigured, None),
            (
                EphemeralReason::Unusable("unexpected state directory /x".into()),
                Some("unexpected state directory /x".to_string()),
            ),
        ] {
            let hits = handle();
            configure(&hits, &Storage::Ephemeral(reason));
            assert_eq!(storage_of(&hits), (false, shown));
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_falls_back_to_memory_and_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().canonicalize().unwrap();
        std::fs::write(state.join(RULE_HITS_FILE), b"garbage").unwrap();
        let hits = handle();
        configure(&hits, &Storage::Persistent(state.clone()));
        let (persistent, reason) = storage_of(&hits);
        assert!(!persistent);
        assert!(reason.unwrap().contains("rule hit counts file"));
        hits.save_now();
        assert_eq!(
            std::fs::read(state.join(RULE_HITS_FILE)).unwrap(),
            b"garbage"
        );
    }
}
