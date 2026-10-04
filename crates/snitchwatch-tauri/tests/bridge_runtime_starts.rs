use snitchwatch_tauri::bridge_runtime::{spawn_bridge_runtime, BridgeRuntimeConfig};

#[tokio::test]
async fn spawned_bridge_publishes_initial_idle_state() {
    // `spawn_bridge_runtime` puts the WS socket + token under
    // `$XDG_RUNTIME_DIR/snitchwatch/`. Without this, `cargo test` on a dev box
    // replaces a running bridge's socket and token (issue #34's mechanism) and
    // leaves that bridge unreachable. One test per binary, so setting the
    // process env here can't race another test.
    let runtime_dir = tempfile::tempdir().expect("tempdir for XDG_RUNTIME_DIR");
    std::env::set_var("XDG_RUNTIME_DIR", runtime_dir.path());

    let cfg = BridgeRuntimeConfig {
        ws_proxy_bind: "127.0.0.1:0".parse().unwrap(),
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let runtime = spawn_bridge_runtime(cfg).await.unwrap();

    assert_eq!(
        *runtime.tray_rx().borrow(),
        snitchwatch_bridge::tray_state::TrayState::Idle
    );
    assert!(
        runtime_dir.path().join("snitchwatch/token").exists(),
        "the test bridge must write its token under the temp runtime dir"
    );

    runtime.shutdown();
}
