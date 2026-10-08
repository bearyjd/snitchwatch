//! Rule export/import on the legacy per-user TCP transport (roadmap P2.7,
//! review M5): any local process can pose as the daemon there (#35), so a
//! GUI's export, preview and apply are all refused, end to end, and nothing
//! reaches the daemon. The flows themselves run on the system (Unix)
//! transport in bridge-cli's `rules_import::e2e_tests`.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::ClientConfig;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<UnixStream>;

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no WS frame in 10 s")
        {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(_)) => {}
            other => panic!("WS ended: {other:?}"),
        }
    }
}

async fn next_action(ws: &mut Ws, action: &str) -> Value {
    loop {
        let v = next_json(ws).await;
        if v["action"] == action {
            return v;
        }
    }
}

#[tokio::test]
async fn import_and_export_are_refused_on_the_tcp_transport() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    })
    .await
    .unwrap();
    let stream = UnixStream::connect(&bridge.ws_socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(bridge.ws_token.as_str().to_string()))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["action"], "authenticated");

    let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    mock.subscribe_with_config(ClientConfig {
        name: "mock".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let (_replies, mut notifications) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .unwrap()
        .unwrap();

    let send = |message: Value| Message::Text(message.to_string());
    ws.send(send(json!({ "action": "exportRules", "requestId": "e" })))
        .await
        .unwrap();
    let refused = next_action(&mut ws, "rulesExportUnavailable").await;
    assert!(refused["reason"]
        .as_str()
        .unwrap()
        .contains("system Snitchwatch service"));

    let document = json!({ "format": "snitchwatch.rules", "version": 1, "rules": [{
        "name": "100-new", "enabled": true, "action": "deny", "duration": "always",
        "operator": { "type": "simple", "operand": "dest.host", "data": "a.example" } }] });
    ws.send(send(
        json!({ "action": "previewRulesImport", "requestId": "p", "document": document }),
    ))
    .await
    .unwrap();
    let refused = next_action(&mut ws, "rulesImportRefused").await;
    assert_eq!(refused["requestId"], "p");
    ws.send(send(
        json!({ "action": "applyRulesImport", "requestId": "a",
                         "previewId": "x", "include": ["100-new"] }),
    ))
    .await
    .unwrap();
    let refused = next_action(&mut ws, "rulesImportRefused").await;
    assert_eq!(refused["requestId"], "a");
    assert!(refused["reason"]
        .as_str()
        .unwrap()
        .contains("system Snitchwatch service"));

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        notifications.try_recv().is_err(),
        "nothing reached the daemon"
    );
    bridge.shutdown();
}
