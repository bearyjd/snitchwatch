//! Connection history is off by default: a bridge started without enabling
//! it creates no `history/` directory and no settings file, in either mode
//! (design D2). Every history PR keeps this test.
//!
//! The in-process bridges use tempdir sockets and state under the target
//! directory, never a runtime dir of the host.

use std::path::Path;
use std::time::Duration;

use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, RunOptions, RunningBridge, Storage,
};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("history-off-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

async fn start(mode: BridgeMode, sockets: &Path, state: &Path) -> RunningBridge {
    run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.to_path_buf()),
            blocklist_fetcher: None,
            mode,
        },
    )
    .await
    .unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            found.push(path.file_name().unwrap().to_string_lossy().into_owned());
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    found
}

async fn creates_no_history(mode: BridgeMode) {
    let sockets = tempdir();
    let state_root = tempdir();
    let state = state_root.path().canonicalize().unwrap();
    let _bridge = start(mode, sockets.path(), &state).await;
    // Give any startup task time to misbehave.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let found = entries(&state);
    assert!(
        !found
            .iter()
            .any(|n| n.contains("history") || n == "settings.json"),
        "history files exist while history is off: {found:?}"
    );
}

#[tokio::test]
async fn a_user_bridge_with_history_off_creates_no_history_directory() {
    creates_no_history(BridgeMode::User).await;
}

#[tokio::test]
async fn a_system_bridge_with_history_off_creates_no_history_directory() {
    creates_no_history(BridgeMode::System).await;
}
