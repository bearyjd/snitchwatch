//! End-to-end test: mock_opensnitchd ↔ bridge ↔ WebSocket client.
//!
//! This is the M1 acceptance test. It proves that:
//!   1. The bridge can connect to a gRPC server.
//!   2. An ask-rule notification envelope (see `downstream.rs` Task 20 note)
//!      produces an `InsertConnectionRows` broadcast to the WS client.
//!   3. A WS `SetVerdict` closes the loop: the mock observes a
//!      `NotificationReply` whose id matches the original notification.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::{MockOpensnitchd, ScriptedEvent};
use serde_json::json;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::{Notification, NotificationReplyCode};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn mock_accepts_tonic_client_connection() {
    let _ = tracing_subscriber::fmt::try_init();

    let mock = MockOpensnitchd::new();
    let ask = Notification::default();
    mock.script(vec![ScriptedEvent::Notification(ask)]).await;
    let mock_addr = mock.spawn().await;

    let endpoint = format!("http://{}", mock_addr);
    let result = tonic::transport::Endpoint::from_shared(endpoint)
        .unwrap()
        .connect()
        .await;
    assert!(
        result.is_ok(),
        "mock should accept a tonic client connection"
    );
}

/// Build a Notification whose `data` field carries the ask-rule JSON envelope
/// the bridge's downstream translator looks for.
fn ask_rule_notification(id: u64, process: &str, host: &str, port: u16) -> Notification {
    let data = json!({
        "kind": "ask_rule",
        "id": id,
        "process": process,
        "process_path": format!("/usr/bin/{process}"),
        "host": host,
        "ip": "93.184.216.34",
        "port": port,
        "protocol": "tcp",
    })
    .to_string();
    Notification {
        id,
        data,
        ..Default::default()
    }
}

#[tokio::test]
async fn ask_rule_round_trip_full() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Spin up the mock with a scripted ask-rule notification.
    let mock = MockOpensnitchd::new();
    let ask = ask_rule_notification(42, "curl", "example.com", 443);
    mock.script(vec![
        // A short delay so the bridge has time to subscribe before the
        // mock tries to send. The mock's outbound task reads the script
        // only after Notifications() is called, so this is belt-and-braces.
        ScriptedEvent::Delay(50),
        ScriptedEvent::Notification(ask),
    ])
    .await;
    let mock_addr = mock.clone().spawn().await;

    // 2. Start the bridge directly via the library entry point.
    let config = BridgeConfig {
        grpc_url: format!("http://{}", mock_addr),
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        cache_capacity: 1024,
    };
    let bridge = run(config).await.expect("bridge run failed");

    // 3. Connect a WebSocket client and wait for InsertConnectionRows.
    let ws_url = format!("ws://{}/stream", bridge.ws_addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect failed");

    let insert_msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).expect("server sent bad json");
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

    // Validate the inserted row.
    let rows = insert_msg
        .get("rows")
        .and_then(|r| r.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.get("id").and_then(|v| v.as_str()), Some("ask-42"));
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

    // 4. Send a SetVerdict to decide the pending row.
    let verdict = json!({
        "action": "setVerdict",
        "rowId": "ask-42",
        "verdict": "allow",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string()))
        .await
        .expect("ws send failed");

    // 5. Poll the mock until it sees the NotificationReply.
    let replies = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let got = mock.received_replies().await;
            if got.iter().any(|r| r.id == 42) {
                return got;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for NotificationReply on the mock");

    let reply = replies
        .iter()
        .find(|r| r.id == 42)
        .expect("no matching reply");
    assert_eq!(
        reply.code,
        NotificationReplyCode::Ok as i32,
        "reply code should be OK"
    );
    assert!(
        reply.data.contains("allow"),
        "reply.data should carry the allow verdict, got {}",
        reply.data
    );

    bridge.shutdown();
}
