//! End to end, prompt-slot plan part A: a held `AskRule` from the mock
//! daemon is announced to a WebSocket GUI as the slot's holder (after its
//! row), the daemon's pings turn into "at least N defaulted", a
//! `RequestSnapshot` repeats the state, and answering releases it. The bridge
//! advertises the `promptSlot` capability. Same boot pattern as
//! `bridge_protocol_test.rs`.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::{Connection, Statistics};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Connects, presents the token, and returns the socket and the ack.
async fn connect(
    socket_path: &std::path::Path,
    token: &str,
) -> (WebSocketStream<UnixStream>, Value) {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(token.to_string())).await.unwrap();
    let ack = next_frame(&mut ws).await;
    (ws, ack)
}

async fn next_frame(ws: &mut WebSocketStream<UnixStream>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("no frame")
}

/// The next frame with `action`, skipping others.
async fn next_action(ws: &mut WebSocketStream<UnixStream>, action: &str) -> Value {
    loop {
        let frame = next_frame(ws).await;
        if frame["action"] == action {
            return frame;
        }
    }
}

fn stats(rule_misses: u64, uptime: u64) -> Statistics {
    Statistics {
        rule_misses,
        uptime,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_held_ask_is_announced_counted_snapshotted_and_released() {
    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    })
    .await
    .expect("bridge run failed");
    let (mut ws, ack) = connect(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;
    assert_eq!(ack["action"], "authenticated");
    assert!(
        ack["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("promptSlot")),
        "{ack}"
    );

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask = tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        daemon
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "updates.example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/background-agent".into(),
                ..Default::default()
            })
            .await
            .unwrap()
    });

    let row_id = next_action(&mut ws, "insertConnectionRows").await["rows"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let held = next_action(&mut ws, "promptSlot").await;
    assert_eq!(held["holder"]["rowId"], row_id.as_str());
    assert_eq!(
        held["holder"]["what"],
        "background-agent → updates.example.com"
    );
    assert_eq!(held["holders"], 1);
    assert_eq!(held["defaultedAtLeast"], Value::Null);

    // The daemon keeps pinging while the prompt is open.
    let mut pinger = MockOpensnitchd::connect(grpc_addr).await.unwrap();
    pinger.ping_with_stats(1, stats(100, 500)).await.unwrap();
    pinger.ping_with_stats(2, stats(104, 510)).await.unwrap();
    assert_eq!(
        next_action(&mut ws, "promptSlot").await["defaultedAtLeast"],
        4
    );

    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .unwrap();
    let snapshot = next_action(&mut ws, "promptSlot").await;
    assert_eq!(snapshot["holder"]["rowId"], row_id.as_str());
    assert_eq!(snapshot["defaultedAtLeast"], 4);

    ws.send(Message::Text(
        json!({
            "action": "setVerdict",
            "rowId": row_id,
            "verdict": "deny",
            "scope": "this_host",
            "duration": "once",
        })
        .to_string(),
    ))
    .await
    .unwrap();
    let rule = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("ask_rule timed out")
        .unwrap();
    assert_eq!(rule.action, "deny");
    let released = next_action(&mut ws, "promptSlot").await;
    assert_eq!(released["holder"], Value::Null);
    assert_eq!(released["holders"], 0);

    bridge.shutdown();
}
