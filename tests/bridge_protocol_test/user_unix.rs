//! #35: the default listener is the per-user Unix socket, never TCP.

use super::*;
use snitchwatch_bridge_cli::{GrpcEndpoint, GrpcListen};
use std::os::unix::fs::MetadataExt;

#[tokio::test]
async fn default_listener_is_a_user_unix_socket_and_ask_rule_round_trips() {
    // The path `from_env` resolves for XDG_RUNTIME_DIR=<tempdir>, built
    // directly so no test touches the process environment.
    let runtime = tempfile::tempdir().unwrap();
    let grpc_path = runtime
        .path()
        .join("snitchwatch-grpc")
        .join("opensnitchd.sock");
    let ws_dir = runtime.path().join("snitchwatch");
    let cfg = BridgeConfig {
        grpc_listen: GrpcListen::UserUnix(grpc_path.clone()),
        ws_socket_path: ws_dir.join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // No TCP listener: the endpoint is the Unix socket.
    assert_eq!(
        bridge.grpc_endpoint,
        GrpcEndpoint::UserUnix(grpc_path.clone())
    );
    assert_eq!(bridge.grpc_endpoint.tcp_addr(), None);
    assert_eq!(
        std::fs::metadata(grpc_path.parent().unwrap())
            .unwrap()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&grpc_path).unwrap().mode() & 0o7777,
        0o600
    );
    assert!(!grpc_path.starts_with(&ws_dir));

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;
    let ask = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect_unix(&grpc_path).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "tcp".into(),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    let row_id = loop {
        let frame = next_server_message(&mut ws).await;
        if let ServerMessage::InsertConnectionRows { rows } = frame {
            break rows[0].id.clone();
        }
    };
    ws.send(Message::Text(
        json!({
            "action": "setVerdict", "rowId": row_id, "verdict": "allow",
            "scope": "this_host", "remember": false,
        })
        .to_string(),
    ))
    .await
    .unwrap();
    let rule = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("ask_rule timed out")
        .unwrap();
    assert_eq!(rule.action, "allow");
    bridge.shutdown();
}
