//! End-to-end, issue #44 second half: mock_opensnitchd asks about a
//! connection the daemon could only describe as `"Kernel connection"`, and a
//! WebSocket GUI answers "Allow, forever". The bridge must reply with a
//! once-only rule the daemon accepts, never announce a remembered rule, and
//! tell the GUI why — the same boot/handshake pattern as
//! `bridge_protocol_test.rs`'s `ask_rule_round_trip_unary`.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::json;
use snitchwatch_bridge::translator::process_binding::RuleRefusal;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::Connection;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Connect to `/stream` and present the handshake token (see
/// `bridge_protocol_test.rs`'s helper of the same name).
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
    let ack = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = ack else {
        panic!("expected authenticated ACK")
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["action"],
        "authenticated"
    );
    ws
}

/// Every JSON frame until `quiet` passes with nothing new.
async fn frames_until_quiet(
    ws: &mut WebSocketStream<UnixStream>,
    quiet: Duration,
) -> Vec<serde_json::Value> {
    let mut frames = Vec::new();
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(quiet, ws.next()).await {
        if let Message::Text(text) = frame {
            frames.push(serde_json::from_str(&text).expect("server sent bad json"));
        }
    }
    frames
}

#[tokio::test]
async fn a_remembered_answer_for_a_kernel_connection_is_answered_once_and_explained() {
    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    })
    .await
    .expect("bridge run failed");
    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "tcp".into(),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            process_path: "Kernel connection".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    let row_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["action"] == "insertConnectionRows" {
                    return v["rows"][0]["id"].as_str().unwrap().to_string();
                }
            }
        }
    })
    .await
    .expect("timed out waiting for insertConnectionRows");

    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "allow",
        "scope": "this_host",
        "duration": "always",
    });
    ws.send(Message::Text(verdict.to_string())).await.unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("ask_rule timed out")
        .expect("ask_rule task panicked");
    assert_eq!(rule.action, "allow");
    assert_eq!(
        rule.duration, "once",
        "never remembered without a program file"
    );
    mock_opensnitchd::validate_rule_shape(&rule).expect("the daemon accepts the once reply");

    let frames = frames_until_quiet(&mut ws, Duration::from_millis(500)).await;
    let explained: Vec<_> = frames
        .iter()
        .filter(|f| f["action"] == "verdictNotRemembered")
        .collect();
    assert_eq!(explained.len(), 1, "{frames:?}");
    assert_eq!(explained[0]["rowId"], row_id);
    assert_eq!(
        explained[0]["reason"],
        RuleRefusal::ProcessFileUnknown.describe()
    );
    assert!(
        !frames.iter().any(|f| f["action"] == "updateRules"),
        "no remembered rule may be announced: {frames:?}"
    );

    bridge.shutdown();
}
