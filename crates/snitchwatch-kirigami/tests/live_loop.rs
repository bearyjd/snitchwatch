//! Live core-loop integration test (code-review item 5): a mock opensnitchd
//! dials a separately running bridge and fires `AskRule`; the pending row flows
//! through exactly the glue the Kirigami shell runs below QML — the feed's
//! routing predicate + JSON encoding (`bridge_dispatch`), a `RowStore` apply
//! (what `ConnectionsModel` does on the Qt thread), and the decision dialog's
//! verdict construction (`pending_decision::build_verdict_message`) — back
//! through the bridge's inbound pump, resolving the daemon's unary `AskRule`.
//!
//! Complements the workspace-root `tests/bridge_protocol_test.rs`, which
//! proves the same loop over the *WebSocket* transport; this test proves the
//! native shell's external WebSocket path with the shell's own Qt-free
//! components in the middle. No Qt objects are
//! instantiated, so it runs fully headless.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_kirigami::bridge_dispatch::{decode_client, encode_server, interests_connections};
use snitchwatch_kirigami::connections::row_store::RowStore;
use snitchwatch_kirigami::pending_decision::build_verdict_message;
use snitchwatch_proto::protocol::Connection;
use tokio::net::UnixStream;
use tokio_tungstenite::{client_async, tungstenite::Message};

#[tokio::test]
async fn verdict_round_trips_through_the_external_websocket_glue() {
    let _ = tracing_subscriber::fmt::try_init();

    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg.clone()).await.expect("bridge run failed");

    // The production client reads the service-owned token and authenticates
    // before requesting its initial snapshot; it never writes either file.
    let token = snitchwatch_bridge::auth::read_token_file(&bridge.ws_token_path).unwrap();
    let stream = UnixStream::connect(&cfg.ws_socket_path).await.unwrap();
    let (mut ws, _) = client_async("ws://localhost/stream", stream).await.unwrap();
    ws.send(Message::Text(token.as_str().to_owned()))
        .await
        .unwrap();
    ws.send(Message::Text(
        serde_json::to_string(&ClientMessage::RequestSnapshot).unwrap(),
    ))
    .await
    .unwrap();

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask = tokio::spawn(async move {
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

    // The model-side half of the feed: route with the real predicate, encode
    // to the JSON `applyServerMessageJson` consumes, decode, and fold into a
    // real `RowStore` — byte-for-byte what `ConnectionsModel` does per message.
    let mut store = RowStore::default();
    let row_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let msg = match ws
                .next()
                .await
                .expect("websocket closed")
                .expect("websocket error")
            {
                Message::Text(text) => serde_json::from_str(&text).expect("server message decode"),
                other => panic!("unexpected websocket frame: {other:?}"),
            };
            if !interests_connections(&msg) {
                continue;
            }
            let json = encode_server(&msg).expect("feed encode failed");
            let typed: ServerMessage =
                serde_json::from_str(&json).expect("feed JSON did not round-trip");
            let pending_id = match &typed {
                ServerMessage::InsertConnectionRows { rows } => rows
                    .iter()
                    .find(|r| r.action.is_none())
                    .map(|r| r.id.clone()),
                _ => None,
            };
            store.apply(typed);
            if let Some(id) = pending_id {
                break id;
            }
        }
    })
    .await
    .expect("no pending connection row was broadcast");
    assert_eq!(
        store.is_pending(&row_id),
        Some(true),
        "row must be pending in the shell-side store before the verdict"
    );

    // The decision dialog's path: UI tokens → typed SetVerdict JSON →
    // decode_client (what the bridge feed's inbound dispatcher runs) →
    // the bridge's inbound pump.
    let verdict =
        build_verdict_message(&row_id, "allow", "this_host", "this_time").expect("verdict build");
    let json = serde_json::to_string(&verdict).expect("verdict serialize");
    let decoded = decode_client(&json).expect("verdict decode");
    ws.send(Message::Text(serde_json::to_string(&decoded).unwrap()))
        .await
        .unwrap();

    // The daemon's blocked AskRule unary resolves with the allow rule.
    let rule = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("AskRule never resolved after verdict")
        .expect("mock daemon task panicked");
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");

    bridge.shutdown();
}
