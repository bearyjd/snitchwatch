//! Every row of the storage failure table (plan A5), without touching the
//! process environment.

use super::*;
use snitchwatch_bridge::blocklists::Enforcement;
use snitchwatch_bridge::cache::rules::RulesSync;
use snitchwatch_bridge::daemon_commands::DaemonTransport;

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
            unreadable: false,
            persistent: true,
            reason: None
        }
    );
    for reason in [EphemeralReason::InProcess, EphemeralReason::NotConfigured] {
        assert_eq!(
            status(Storage::Ephemeral(reason)),
            StorageStatus {
                unreadable: false,
                persistent: false,
                reason: None
            }
        );
    }
    assert_eq!(
        status(unusable("blocklist store: locked".into())),
        StorageStatus {
            unreadable: false,
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

/// The daemon handles a test manager's sink would send through.
fn daemon() -> DaemonRules {
    let rules = RulesSync::new(tokio::sync::broadcast::channel(4).0);
    DaemonRules {
        commands: DaemonCommands::new(DaemonTransport::Tcp, rules.clone()),
        rules: rules.cache(),
    }
}

#[test]
fn the_manager_reports_the_resolved_storage() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    let persistent = build_blocklists_manager(
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
        daemon(),
    )
    .unwrap();
    assert!(persistent.storage_status().persistent);

    std::fs::remove_file(state.join(BLOCKLIST_DB_FILE)).unwrap();
    std::fs::create_dir(state.join(BLOCKLIST_DB_FILE)).unwrap();
    let fallback = build_blocklists_manager(
        RunOptions {
            storage: Storage::Persistent(state),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
        daemon(),
    )
    .unwrap();
    assert!(!fallback.storage_status().persistent);
    assert!(fallback
        .storage_status()
        .reason
        .as_deref()
        .is_some_and(|r| r.starts_with("blocklist store: ")));

    let in_process = build_blocklists_manager(RunOptions::in_process(), daemon()).unwrap();
    assert_eq!(RunOptions::in_process().mode, BridgeMode::User);
    assert!(!in_process.storage_status().persistent);
}

fn not_enforced(manager: &BlocklistsManager) -> String {
    match manager.enforcement("any") {
        Enforcement::NotEnforced { reason } => reason,
        other => panic!("expected NotEnforced, got {other:?}"),
    }
}

/// Issue #45 PR B (B0): without a state directory nothing is installed and
/// every list says why; no list directory is created anywhere.
#[test]
fn ephemeral_storage_installs_nothing_and_says_why() {
    for (storage, reason) in [
        (
            Storage::Ephemeral(EphemeralReason::InProcess),
            "no state directory: in-process",
        ),
        (
            Storage::Ephemeral(EphemeralReason::NotConfigured),
            "no state directory: not configured",
        ),
        (
            Storage::Ephemeral(EphemeralReason::Unusable(
                "unexpected state directory /x".into(),
            )),
            "no state directory: unexpected state directory /x",
        ),
    ] {
        let manager = build_blocklists_manager(
            RunOptions {
                storage,
                blocklist_fetcher: None,
                mode: BridgeMode::System,
            },
            daemon(),
        )
        .unwrap();
        assert_eq!(not_enforced(&manager), reason);
    }

    // A state directory whose database can't open is Unusable too: no list
    // files go next to it.
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    std::fs::create_dir(state.join(BLOCKLIST_DB_FILE)).unwrap();
    let fallback = build_blocklists_manager(
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
            mode: BridgeMode::System,
        },
        daemon(),
    )
    .unwrap();
    assert!(not_enforced(&fallback).starts_with("no state directory: blocklist store: "));
    assert!(!state.join("blocklists").exists());
}

#[test]
fn the_system_bridge_creates_the_private_list_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    let manager = build_blocklists_manager(
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
            mode: BridgeMode::System,
        },
        daemon(),
    )
    .unwrap();
    assert_eq!(manager.enforcement("any"), Enforcement::Pending);
    let mode = std::fs::metadata(state.join("blocklists"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o700);
}

/// The system bridge's state directory must be the service account's own,
/// mode 0700 (`StateDirectoryMode=0700`): root opensnitchd reads list files
/// under it.
#[test]
fn the_system_state_directory_must_be_the_services_and_private() {
    let ok = DirFacts {
        uid: 991,
        gid: 991,
        mode: 0o40700,
    };
    assert_eq!(check_system_state_dir(&ok, 991, 991), Ok(()));
    for (facts, why) in [
        (DirFacts { uid: 0, ..ok }, "owner"),
        (DirFacts { gid: 0, ..ok }, "group"),
        (
            DirFacts {
                mode: 0o40750,
                ..ok
            },
            "group-readable",
        ),
        (
            DirFacts {
                mode: 0o40701,
                ..ok
            },
            "world bit",
        ),
    ] {
        let err = check_system_state_dir(&facts, 991, 991).unwrap_err();
        assert!(err.contains(SYSTEM_STATE_DIR), "{why}: {err}");
    }
}

/// Review M3: root opensnitchd would read per-user list files that any of
/// the user's processes can replace (a FIFO hangs it, a link to /dev/zero
/// exhausts its memory and, with `QueueBypass`, fails open). Only the system
/// bridge installs blocklist rules; a per-user one writes no list file.
#[test]
fn the_per_user_bridge_never_installs_blocklist_rules() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    let manager = build_blocklists_manager(
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
        daemon(),
    )
    .unwrap();
    assert!(
        manager.storage_status().persistent,
        "subscriptions are still saved"
    );
    assert_eq!(not_enforced(&manager), PER_USER_REASON);
    assert!(!PER_USER_REASON.contains("bridge"), "plain language");
    assert!(!state.join("blocklists").exists());
}

// ---- a per-user state directory -------------------------------------------

fn dir_with_mode(mode: u32) -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(mode)).unwrap();
    let state = state.canonicalize().unwrap();
    (dir, state)
}

/// The state directory holds the databases and the SQLite files beside them;
/// a per-user bridge's directory must be the bridge user's own and not
/// writable by anyone else, as the system bridge's already has to be.
#[test]
fn a_per_user_state_directory_others_can_write_is_unusable() {
    for mode in [0o775, 0o770, 0o757, 0o707, 0o777, 0o722, 0o702] {
        let (_dir, state) = dir_with_mode(mode);
        for storage in [
            resolve_storage_from(None, os(&state), BridgeMode::User),
            resolve_storage_from(os(&state), None, BridgeMode::User),
        ] {
            let reason = unusable_reason(&storage);
            assert!(
                reason.contains("written by other users"),
                "{mode:o}: {reason}"
            );
            assert!(
                reason.contains(state.to_str().unwrap()),
                "{mode:o}: {reason}"
            );
            assert!(reason.contains("chmod"), "the fix is spelled out: {reason}");
        }
    }
}

#[test]
fn a_per_user_state_directory_without_group_or_other_write_is_fine() {
    for mode in [0o700, 0o750, 0o755, 0o705, 0o500] {
        let (_dir, state) = dir_with_mode(mode);
        assert_eq!(
            resolve_storage_from(None, os(&state), BridgeMode::User),
            Storage::Persistent(state),
            "{mode:o}"
        );
    }
}

#[test]
fn a_per_user_state_directory_must_be_the_bridge_users_own() {
    let facts = DirFacts {
        uid: 1000,
        gid: 1000,
        mode: 0o40700,
    };
    let dir = Path::new("/home/u/.local/share/snitchwatch");
    assert_eq!(check_user_state_dir(&facts, 1000, dir), Ok(()));
    // The group and mode of the system check do not apply: only the owner
    // and the write bits.
    assert_eq!(
        check_user_state_dir(
            &DirFacts {
                gid: 5,
                mode: 0o40755,
                ..facts
            },
            1000,
            dir
        ),
        Ok(())
    );
    let other = check_user_state_dir(&DirFacts { uid: 0, ..facts }, 1000, dir).unwrap_err();
    assert!(
        other.contains("not owned by the user running the bridge"),
        "{other}"
    );
    assert!(other.contains(dir.to_str().unwrap()), "{other}");
    let writable = check_user_state_dir(
        &DirFacts {
            mode: 0o40770,
            ..facts
        },
        1000,
        dir,
    )
    .unwrap_err();
    assert!(writable.contains("written by other users"), "{writable}");
}

/// The system bridge keeps its own, stricter rule.
#[test]
fn system_mode_still_uses_the_system_check() {
    let (_dir, state) = dir_with_mode(0o775);
    let storage = resolve_storage_from(None, os(&state), BridgeMode::System);
    assert!(unusable_reason(&storage).contains("unexpected state directory"));
}

// ---- a store file SQLite would hang on -------------------------------------

/// Run `f` on its own thread, failing rather than hanging if it never returns.
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

fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
}

/// A FIFO named `<db>-journal` used to hang SQLite at startup; the store now
/// falls back to memory like any other store that can't be opened.
#[test]
fn a_fifo_beside_the_blocklist_database_falls_back_to_memory_without_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    mkfifo(&state.join(format!("{BLOCKLIST_DB_FILE}-journal")));
    let (store, storage) =
        within(move || open_blocklist_store(Storage::Persistent(state)).unwrap());
    let reason = unusable_reason(&storage);
    assert!(reason.starts_with("blocklist store: "), "{reason}");
    assert!(reason.contains("-journal"), "{reason}");
    assert!(store.list_subscriptions().unwrap().is_empty());
}

#[test]
fn a_symlinked_or_crafted_blocklist_database_falls_back_to_memory() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap();
    let db = state.join(BLOCKLIST_DB_FILE);
    std::os::unix::fs::symlink(state.join("elsewhere"), format!("{}-wal", db.display())).unwrap();
    let (_, storage) = within({
        let state = state.clone();
        move || open_blocklist_store(Storage::Persistent(state)).unwrap()
    });
    assert!(unusable_reason(&storage).starts_with("blocklist store: "));

    std::fs::remove_file(format!("{}-wal", db.display())).unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE VIEW subscriptions AS WITH RECURSIVE c(x) AS \
             (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT x FROM c;",
        )
        .unwrap();
    let (store, storage) =
        within(move || open_blocklist_store(Storage::Persistent(state)).unwrap());
    assert!(unusable_reason(&storage).contains("view named subscriptions"));
    assert!(store.list_subscriptions().unwrap().is_empty());
}
