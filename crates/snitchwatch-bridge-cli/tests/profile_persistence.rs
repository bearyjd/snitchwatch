//! Issue #46 Part 1: profiles and the active-profile choice persist in the
//! resolved state directory (`profiles.sqlite3`), driven through
//! `run_with_options` exactly like `blocklist_wiring.rs`:
//! - a profile and its activation survive a restart on the same directory;
//! - `SetProfiles.storage` reports the profile store's own storage, apart
//!   from the blocklist store's;
//! - an in-process `run()` and an unusable state directory keep profiles in
//!   memory and say so.
//!
//! Every profile here has no network matchers: `connect_watcher()` reaches a
//! real NetworkManager when one exists, and an auto-switch must never decide
//! what these tests see.

use std::path::Path;
use std::time::Duration;

use snitchwatch_bridge::ws_messages::{
    ClientMessage, ProfileSummary, ServerMessage, StorageStatus,
};
use snitchwatch_bridge_cli::{
    run, run_with_options, BridgeConfig, BridgeMode, EphemeralReason, RunOptions, RunningBridge,
    Storage,
};
use tokio::sync::broadcast;

const WAIT: Duration = Duration::from_secs(10);
const PROFILE_DB: &str = "profiles.sqlite3";

fn config(dir: &Path) -> BridgeConfig {
    BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.join("bridge.sock"),
        cache_capacity: 64,
    }
}

fn options(storage: Storage) -> RunOptions {
    RunOptions {
        storage,
        blocklist_fetcher: None,
        mode: BridgeMode::User,
    }
}

/// Ask for a snapshot and return its `SetProfiles` and the `SetBlocklists`
/// storage sent just before it. A `SetProfiles` the profile event pump sent
/// before the snapshot's `SetBlocklists` is skipped.
async fn snapshot(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> (
    Vec<ProfileSummary>,
    Option<StorageStatus>,
    Option<StorageStatus>,
) {
    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        let mut blocklist_storage = None;
        loop {
            match rx.recv().await.expect("broadcast closed") {
                ServerMessage::SetBlocklists { storage, .. } => blocklist_storage = Some(storage),
                ServerMessage::SetProfiles { profiles, storage } => {
                    if let Some(blocklists) = blocklist_storage.take() {
                        return (profiles, storage, blocklists);
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("no SetProfiles in the snapshot")
}

/// Snapshot until the profile list satisfies `done`.
async fn snapshot_until(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&[ProfileSummary]) -> bool,
) -> (Vec<ProfileSummary>, Option<StorageStatus>) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (profiles, storage, _) = snapshot(bridge, rx).await;
        if done(&profiles) {
            return (profiles, storage);
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Create a profile with no network matchers (see the module docs).
async fn create_profile(bridge: &RunningBridge, id: &str, name: &str) {
    bridge
        .inbound_tx
        .send(ClientMessage::CreateProfile {
            id: id.into(),
            name: name.into(),
            network_matchers: vec![],
        })
        .await
        .unwrap();
}

fn persistent() -> Option<StorageStatus> {
    Some(StorageStatus {
        unreadable: false,
        persistent: true,
        reason: None,
    })
}

/// Create two profiles, activate one, restart on the same state directory:
/// both are still there and the same one is active.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profiles_and_the_active_profile_persist_across_a_restart() {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();

    let bridge = run_with_options(
        config(sockets.path()),
        options(Storage::Persistent(state.clone())),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    create_profile(&bridge, "home", "At Home").await;
    create_profile(&bridge, "office", "Office").await;
    bridge
        .inbound_tx
        .send(ClientMessage::ActivateProfile { id: "home".into() })
        .await
        .unwrap();
    let (_, storage) = snapshot_until(&bridge, &mut rx, "the activation", |p| {
        p.iter().any(|p| p.id == "home" && p.active)
    })
    .await;
    assert_eq!(storage, persistent());
    bridge.shutdown();
    assert!(state.join(PROFILE_DB).is_file());

    let bridge = run_with_options(config(sockets.path()), options(Storage::Persistent(state)))
        .await
        .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (restored, storage, _) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    assert_eq!(storage, persistent());
    let summary: Vec<(&str, &str, bool)> = restored
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str(), p.active))
        .collect();
    assert_eq!(
        summary,
        vec![("home", "At Home", true), ("office", "Office", false)]
    );
}

/// `run()` is in-process: profiles are kept in memory, with no reason to
/// show (nothing was misconfigured).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_keeps_profiles_in_memory() {
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run(config(sockets.path())).await.unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (_, storage, _) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    assert_eq!(
        storage,
        Some(StorageStatus {
            unreadable: false,
            persistent: false,
            reason: None
        })
    );
}

/// A directory where `profiles.sqlite3` should be: the profile store can't
/// open, the bridge still starts with profiles in memory and says why, and
/// the blocklist store in the same directory is unaffected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unopenable_profile_store_falls_back_to_memory_on_its_own() {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    std::fs::create_dir(state.join(PROFILE_DB)).unwrap();

    let bridge = run_with_options(config(sockets.path()), options(Storage::Persistent(state)))
        .await
        .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    create_profile(&bridge, "home", "At Home").await;
    let (_, storage) = snapshot_until(&bridge, &mut rx, "the in-memory profile", |p| {
        p.iter().any(|p| p.id == "home")
    })
    .await;
    let (_, _, blocklists) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    let storage = storage.expect("storage status");
    assert!(!storage.persistent, "{storage:?}");
    assert!(
        storage
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("profile store: ")),
        "{storage:?}"
    );
    assert_eq!(blocklists, persistent(), "tracked apart from blocklists");
}

/// A state directory that resolved as unusable: its reason reaches the
/// Profiles page too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unusable_state_directory_is_shown_on_the_profiles_page() {
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        config(sockets.path()),
        options(Storage::Ephemeral(EphemeralReason::Unusable(
            "unexpected state directory /x".into(),
        ))),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (_, storage, _) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    assert_eq!(
        storage,
        Some(StorageStatus {
            unreadable: false,
            persistent: false,
            reason: Some("unexpected state directory /x".into())
        })
    );
}
