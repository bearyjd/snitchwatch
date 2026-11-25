//! PR 10 (#418) through the real bridge entry point: the writer lock lasts
//! as long as the running bridge, not just until it has started. (That it
//! is released once the writer is gone is tested at the gate.)

use snitchwatch_bridge::history::control::{ControlGate, Limits, Refused};
use snitchwatch_bridge::history::{enable, locate, Consent, Mode};
use snitchwatch_bridge_cli::{run_with_options, BridgeConfig, BridgeMode, RunOptions, Storage};

#[tokio::test]
async fn a_running_bridge_keeps_the_writer_lock_for_as_long_as_it_runs() {
    let state = tempfile::Builder::new()
        .prefix("history-lock-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let root = state.path().canonicalize().unwrap();
    let location = locate(Mode::User, Some(&root)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "test".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        BridgeConfig::user_unix_for_tests(sockets.path()),
        RunOptions {
            storage: Storage::Persistent(root.clone()),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
    )
    .await
    .unwrap();

    let second = ControlGate::start(Mode::User, Some(&root), Limits::default());
    assert!(
        second.slot().get().is_none(),
        "a second bridge must not record into a running bridge's history"
    );
    assert!(matches!(second.preview(), Err(Refused::Unavailable(_))));
    drop(second);
    // Held until the bridge's own tasks let go of the capture; there is no
    // promise about the moment, so nothing here waits for it.
    drop(bridge);
}
