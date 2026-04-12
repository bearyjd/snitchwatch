use snitchwatch_tauri::bridge_runtime::{spawn_bridge_runtime, BridgeRuntimeConfig};

#[tokio::test]
async fn spawned_bridge_publishes_initial_idle_state() {
    let cfg = BridgeRuntimeConfig {
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let runtime = spawn_bridge_runtime(cfg).await.unwrap();

    assert_eq!(
        *runtime.tray_rx().borrow(),
        snitchwatch_bridge::tray_state::TrayState::Idle
    );

    runtime.shutdown();
}
