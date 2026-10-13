//! End-to-end persistence regression (LS redesign phase 1): a GUI that answers
//! Allow, `this_host`, `always` for a connection with an absolute program
//! path must get an AskRule reply whose rule duration is `"always"` — the only
//! duration the daemon writes to disk. Same boot pattern as
//! `bridge_protocol_test.rs`'s `ask_rule_round_trip_inline_deny_until_restart`.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::json;
use snitchwatch_bridge::ws_messages::ServerMessage;
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

async fn next_server_message(ws: &mut WebSocketStream<UnixStream>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text).expect("server sent bad json");
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for a WS message")
}

#[tokio::test]
async fn allow_forever_for_an_absolute_path_reaches_the_daemon_as_always() {
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
    let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
    mock.subscribe("mock").await.unwrap();
    let (_reply_tx, _notifications) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .expect("the daemon stream never said HELLO")
        .unwrap();
    let ask = tokio::spawn(async move {
        mock.ask_rule(Connection {
            protocol: "tcp".into(),
            dst_host: "github.com".into(),
            dst_ip: "140.82.112.3".into(),
            dst_port: 443,
            process_path: "/usr/bin/firefox".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    let row_id = loop {
        if let ServerMessage::InsertConnectionRows { rows } = next_server_message(&mut ws).await {
            break rows[0].id.clone();
        }
    };
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
    assert_eq!(
        (rule.action.as_str(), rule.duration.as_str()),
        ("allow", "always")
    );
    mock_opensnitchd::validate_rule_shape(&rule).expect("a real daemon would reject this rule");

    bridge.shutdown();
}
