//! End to end, issue #78: a GUI pauses filtering while the mock daemon's
//! `AskRule` is waiting. The bridge answers it Allow once (the daemon gets a
//! `once` rule, and no rule is saved or sent to GUIs) and sends the row
//! labelled `autoAnswer: "filterPaused"`. Same boot pattern as
//! `prompt_slot_test.rs`.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::Connection;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

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

/// Frames up to and including the next one with `action`.
async fn frames_until(ws: &mut WebSocketStream<UnixStream>, action: &str) -> Vec<Value> {
    let mut frames = Vec::new();
    loop {
        let frame = next_frame(ws).await;
        let done = frame["action"] == action;
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

#[tokio::test]
async fn pausing_answers_a_waiting_ask_allow_once_and_labels_its_row() {
    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    })
    .await
    .expect("bridge run failed");
    let stream = UnixStream::connect(&bridge.ws_socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(bridge.ws_token.as_str().to_string()))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut ws).await["action"], "authenticated");

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
    let inserted = frames_until(&mut ws, "insertConnectionRows").await;
    let row = &inserted.last().unwrap()["rows"][0];
    assert_eq!(row["action"], Value::Null, "the Ask is waiting");
    let row_id = row["id"].as_str().unwrap().to_string();

    ws.send(Message::Text(
        json!({ "action": "setFilteringPaused", "paused": true, "durationSecs": 300 }).to_string(),
    ))
    .await
    .unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("the waiting Ask was not answered by the pause")
        .unwrap();
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");

    // The answered row, then the pause state, then the slot's release (sent
    // once the Ask's reply is built, so after any rule it would have saved).
    let mut frames = frames_until(&mut ws, "updateConnectionRows").await;
    let updated = &frames.last().unwrap()["rows"][0];
    assert_eq!(updated["id"], row_id.as_str());
    assert_eq!(updated["action"], "allow");
    assert_eq!(updated["autoAnswer"], "filterPaused");
    frames.extend(frames_until(&mut ws, "filterPauseState").await);
    loop {
        let frame = next_frame(&mut ws).await;
        let released = frame["action"] == "promptSlot" && frame["holders"] == 0;
        frames.push(frame);
        if released {
            break;
        }
    }
    assert!(
        !frames.iter().any(|f| f["action"] == "updateRules"),
        "a once answer must not be saved as a rule: {frames:?}"
    );

    bridge.shutdown();
}
