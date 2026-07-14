//! End-to-end test: mock_opensnitchd (gRPC client) ↔ bridge (gRPC server +
//! WS server) ↔ WebSocket UI client.
//!
//! This is the M1.5 acceptance test. It proves that:
//!   1. The bridge binds both its gRPC and WebSocket ports.
//!   2. opensnitchd (mocked) can dial in and call `AskRule`.
//!   3. The bridge broadcasts an `InsertConnectionRows` to any connected WS
//!      client carrying the new pending row.
//!   4. A WS `setVerdict` resolves the pending row, and the original
//!      `AskRule` unary call returns a `Rule` whose `action` matches.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::json;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::Connection;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Connect to the bridge's `/stream` route over its Unix domain socket and
/// present the handshake token as the first frame, exactly as a real client
/// (or `crate::loopback_proxy` on the Tauri shell's behalf) must.
async fn connect_stream(socket_path: &std::path::Path, token: &str) -> WebSocketStream<UnixStream> {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("unix socket connect failed");
    let (mut ws, _resp) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .expect("ws handshake failed");
    ws.send(Message::Text(token.to_string()))
        .await
        .expect("token handshake send failed");
    ws
}

#[tokio::test]
async fn ask_rule_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Boot the bridge: ephemeral gRPC port + a Unix socket under a fresh
    //    temp dir for the WS server.
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // 2. Connect a WebSocket client (presenting the handshake token) BEFORE
    //    the AskRule call so we don't miss the broadcast.
    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    // 3. Spawn an opensnitchd mock client and fire AskRule in the background.
    let grpc_addr = bridge.grpc_addr;
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
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

    // 4. Wait for the InsertConnectionRows broadcast on the WS.
    let insert_msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value =
                        serde_json::from_str(&t).expect("server sent bad json");
                    if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                        break v;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for insertConnectionRows");

    let rows = insert_msg
        .get("rows")
        .and_then(|r| r.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let row_id = row
        .get("id")
        .and_then(|v| v.as_str())
        .expect("row id")
        .to_string();
    assert_eq!(row.get("process").and_then(|v| v.as_str()), Some("curl"));
    assert_eq!(
        row.get("dstHost").and_then(|v| v.as_str()),
        Some("example.com")
    );
    assert_eq!(row.get("dstPort").and_then(|v| v.as_u64()), Some(443));
    assert!(
        row.get("action").map(|v| v.is_null()).unwrap_or(false),
        "pending rows must have action: null"
    );

    // 5. Send a SetVerdict to decide the pending row.
    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "allow",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string()))
        .await
        .expect("ws send failed");

    // 6. The mock's AskRule call should now return with action=allow.
    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .expect("ask_rule timed out")
        .expect("ask_rule task panicked");
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(!rule.name.is_empty(), "daemon rejects empty rule names");

    bridge.shutdown();
}

#[tokio::test]
async fn deny_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_addr;
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "udp".into(),
            dst_host: "tracker.bad".into(),
            dst_ip: "1.2.3.4".into(),
            dst_port: 53,
            process_path: "/usr/bin/dnsmasq".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    // Drain WS until we see the insert.
    let row_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                    return v["rows"][0]["id"].as_str().unwrap().to_string();
                }
            }
        }
    })
    .await
    .unwrap();

    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "deny",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string())).await.unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "deny");

    bridge.shutdown();
}

#[tokio::test]
async fn diagnostics_report_reflects_firewall_down_after_subscribe() {
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_addr;
    let subscribe_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.subscribe_with_config(snitchwatch_proto::protocol::ClientConfig {
            id: 1,
            name: "mock".to_string(),
            version: "mock-1.6.0".to_string(),
            is_firewall_running: false,
            ..Default::default()
        })
        .await
        .unwrap();
    });
    subscribe_handle.await.unwrap();

    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .expect("send requestSnapshot failed");

    let mut saw_firewall_failed = false;
    for _ in 0..20 {
        let Some(Ok(Message::Text(text))) = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("timed out waiting for a WS message")
        else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        if v.get("action").and_then(|a| a.as_str()) == Some("diagnosticsReport") {
            let checks = v["checks"].as_array().unwrap();
            saw_firewall_failed = checks
                .iter()
                .any(|c| c["kind"] == "firewall_running" && c["status"]["status"] == "failed");
            break;
        }
    }
    assert!(
        saw_firewall_failed,
        "expected a diagnosticsReport with a failed firewall_running check"
    );

    bridge.shutdown();
}
