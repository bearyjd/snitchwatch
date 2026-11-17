use snitchwatch_tauri::bridge_runtime::{spawn_bridge_runtime, BridgeRuntimeConfig};

#[tokio::test]
async fn spawn_without_the_dev_flag_refuses_and_writes_nothing() {
    // One test per binary, so the process env can't race another test.
    let runtime_dir = tempfile::tempdir().expect("tempdir for XDG_RUNTIME_DIR");
    std::env::set_var("XDG_RUNTIME_DIR", runtime_dir.path());
    std::env::remove_var("SNITCHWATCH_DEV_TCP");
    std::env::remove_var("SNITCHWATCH_GRPC_BIND");
    std::env::remove_var("SNITCHWATCH_SYSTEM_BRIDGE");

    let cfg = BridgeRuntimeConfig {
        ws_proxy_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let err = match spawn_bridge_runtime(cfg).await {
        Ok(_) => panic!("the embed must refuse to start without SNITCHWATCH_DEV_TCP=1"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("SNITCHWATCH_DEV_TCP=1"), "{err}");
    assert!(
        !runtime_dir.path().join("snitchwatch").exists(),
        "a refused start must not create the WS directory or token"
    );
}
