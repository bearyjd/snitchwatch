//! Every row of the storage failure table (plan A5), without touching the
//! process environment.

use super::*;

fn os(path: &Path) -> Option<OsString> {
    Some(path.as_os_str().to_owned())
}

fn unusable_reason(storage: &Storage) -> &str {
    match storage {
        Storage::Ephemeral(EphemeralReason::Unusable(reason)) => reason,
        other => panic!("expected Unusable, got {other:?}"),
    }
}

#[test]
fn nothing_configured_is_not_configured() {
    for mode in [BridgeMode::User, BridgeMode::System] {
        assert_eq!(
            resolve_storage_from(None, None, mode),
            Storage::Ephemeral(EphemeralReason::NotConfigured)
        );
        assert_eq!(
            resolve_storage_from(Some("".into()), Some("".into()), mode),
            Storage::Ephemeral(EphemeralReason::NotConfigured),
            "empty values count as unset"
        );
    }
}

#[test]
fn an_existing_directory_is_persistent_and_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("link");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        resolve_storage_from(None, os(&link), BridgeMode::User),
        Storage::Persistent(real.canonicalize().unwrap())
    );
    assert_eq!(
        resolve_storage_from(os(&link), None, BridgeMode::User),
        Storage::Persistent(real.canonicalize().unwrap())
    );
}

#[test]
fn state_directory_wins_over_snitchwatch_state_dir() {
    let systemd = tempfile::tempdir().unwrap();
    let manual = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_storage_from(os(systemd.path()), os(manual.path()), BridgeMode::User),
        Storage::Persistent(systemd.path().canonicalize().unwrap())
    );
    // An empty STATE_DIRECTORY is unset, so the override applies.
    assert_eq!(
        resolve_storage_from(Some("".into()), os(manual.path()), BridgeMode::User),
        Storage::Persistent(manual.path().canonicalize().unwrap())
    );
}

/// A broken `$STATE_DIRECTORY` is reported, not silently replaced by the
/// other variable.
#[test]
fn an_unusable_state_directory_does_not_fall_back() {
    let manual = tempfile::tempdir().unwrap();
    let missing = manual.path().join("missing");
    let storage = resolve_storage_from(os(&missing), os(manual.path()), BridgeMode::User);
    assert!(unusable_reason(&storage).contains("state directory"));
}

#[test]
fn a_missing_directory_is_unusable() {
    let dir = tempfile::tempdir().unwrap();
    let storage = resolve_storage_from(None, os(&dir.path().join("nope")), BridgeMode::User);
    let reason = unusable_reason(&storage);
    assert!(reason.starts_with("state directory "), "{reason}");
    assert!(reason.contains("nope"), "{reason}");
}

#[test]
fn a_file_is_not_a_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, b"").unwrap();
    let storage = resolve_storage_from(None, os(&file), BridgeMode::User);
    assert!(unusable_reason(&storage).contains("not a directory"));
}

/// The system bridge runs as `snitchwatch` and its directory is read by root
/// opensnitchd (PR B): it accepts exactly `/var/lib/snitchwatch`.
#[test]
fn system_mode_accepts_only_var_lib_snitchwatch() {
    let dir = tempfile::tempdir().unwrap();
    for configured in [
        (os(dir.path()), None),
        (None, os(dir.path())),
        (Some("/tmp".into()), None),
    ] {
        let storage = resolve_storage_from(configured.0, configured.1, BridgeMode::System);
        assert!(
            unusable_reason(&storage).starts_with("unexpected state directory "),
            "{storage:?}"
        );
    }
    // The same directory is fine for the per-user bridge.
    assert!(matches!(
        resolve_storage_from(os(dir.path()), None, BridgeMode::User),
        Storage::Persistent(_)
    ));
}

#[test]
fn only_an_unusable_reason_reaches_the_user() {
    let status = |s: Storage| s.status();
    assert_eq!(
        status(Storage::Persistent("/x".into())),
        StorageStatus {
            persistent: true,
            reason: None
        }
    );
    for reason in [EphemeralReason::InProcess, EphemeralReason::NotConfigured] {
        assert_eq!(
            status(Storage::Ephemeral(reason)),
            StorageStatus {
                persistent: false,
                reason: None
            }
        );
    }
    assert_eq!(
        status(unusable("blocklist store: locked".into())),
        StorageStatus {
            persistent: false,
            reason: Some("blocklist store: locked".into())
        }
    );
}

#[test]
fn in_process_runs_are_never_persistent() {
    let options = RunOptions::in_process();
    assert_eq!(
        options.storage,
        Storage::Ephemeral(EphemeralReason::InProcess)
    );
    assert!(options.blocklist_fetcher.is_none());
}

#[test]
fn a_persistent_store_is_written_in_the_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    let (_store, storage) = open_blocklist_store(Storage::Persistent(state.clone())).unwrap();
    assert_eq!(storage, Storage::Persistent(state.clone()));
    assert!(state.join(BLOCKLIST_DB_FILE).is_file());
}

/// A store that can't be opened falls back to memory and says why; the
/// bridge still starts.
#[test]
fn an_unopenable_store_falls_back_to_memory() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    std::fs::create_dir(state.join(BLOCKLIST_DB_FILE)).unwrap();
    let (store, storage) = open_blocklist_store(Storage::Persistent(state)).unwrap();
    assert!(unusable_reason(&storage).starts_with("blocklist store: "));
    assert!(store.list_subscriptions().unwrap().is_empty());
}

#[test]
fn the_manager_reports_the_resolved_storage() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    let persistent = build_blocklists_manager(RunOptions {
        storage: Storage::Persistent(state.clone()),
        blocklist_fetcher: None,
    })
    .unwrap();
    assert!(persistent.storage_status().persistent);

    std::fs::remove_file(state.join(BLOCKLIST_DB_FILE)).unwrap();
    std::fs::create_dir(state.join(BLOCKLIST_DB_FILE)).unwrap();
    let fallback = build_blocklists_manager(RunOptions {
        storage: Storage::Persistent(state),
        blocklist_fetcher: None,
    })
    .unwrap();
    assert!(!fallback.storage_status().persistent);
    assert!(fallback
        .storage_status()
        .reason
        .as_deref()
        .is_some_and(|r| r.starts_with("blocklist store: ")));

    let in_process = build_blocklists_manager(RunOptions::in_process()).unwrap();
    assert!(!in_process.storage_status().persistent);
}
