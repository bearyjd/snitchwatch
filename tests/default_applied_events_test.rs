//! End to end, E3 (plan `2026-10-08-default-applied-events.md`): the
//! bazzite-tower fork reports connections its `DefaultAction` decided as
//! `Statistics.events[]` entries with a marked synthetic rule. The mock
//! daemon pings with such events; a WebSocket GUI gets them as decided rows
//! that say the default decided them, not a rule named "", and the hit
//! counts report no gap, because those events grew `rule_misses`, not
//! `rule_hits`. Same boot pattern as `bridge_protocol_test.rs`.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::{default_action_event, MockOpensnitchd};
use serde_json::{json, Value};
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::{Connection, Event, Rule, Statistics};
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

fn conn(host: &str) -> Connection {
    Connection {
        protocol: "tcp".into(),
        dst_host: host.into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: "/usr/bin/background-agent".into(),
        ..Default::default()
    }
}

fn rule_event(name: &str, host: &str, unixnano: i64) -> Event {
    Event {
        connection: Some(conn(host)),
        rule: Some(Rule {
            name: name.into(),
            action: "allow".into(),
            duration: "always".into(),
            enabled: true,
            ..Default::default()
        }),
        unixnano,
        ..Default::default()
    }
}

#[tokio::test]
async fn default_applied_events_reach_the_gui_as_default_decided_rows_without_a_gap() {
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

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let mut daemon = MockOpensnitchd::connect(grpc_addr).await.unwrap();
    const S: i64 = 1_000_000_000;

    // The baseline: one rule hit and one default-applied allow.
    daemon
        .ping_with_stats(
            1,
            Statistics {
                uptime: 100,
                rule_hits: 10,
                rule_misses: 3,
                events: vec![
                    rule_event("899-agent-allow", "rule.example.com", 1_800_000_000 * S),
                    default_action_event(conn("first.example.com"), "allow", 1_800_000_001 * S),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rows = next_action(&mut ws, "insertConnectionRows").await["rows"].clone();
    assert_eq!(rows[0]["matchedRule"], "899-agent-allow", "{rows}");
    assert!(rows[0].get("decidedByDefault").is_none(), "{rows}");
    assert_eq!(rows[1]["action"], "allow", "{rows}");
    assert_eq!(rows[1]["decidedByDefault"], true, "{rows}");

    // Only default-applied connections: `rule_hits` stays, `rule_misses`
    // grows by them.
    daemon
        .ping_with_stats(
            2,
            Statistics {
                uptime: 110,
                rule_hits: 10,
                rule_misses: 5,
                events: vec![
                    default_action_event(conn("second.example.com"), "deny", 1_800_000_002 * S),
                    default_action_event(conn("third.example.com"), "reject", 1_800_000_003 * S),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rows = next_action(&mut ws, "insertConnectionRows").await["rows"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{rows}");
    for row in rows.as_array().unwrap() {
        assert_eq!(row["action"], "deny", "{row}");
        assert_eq!(row["decidedByDefault"], true, "{row}");
        assert!(
            row.get("matchedRule").is_none(),
            "no rule named \"\": {row}"
        );
        assert!(row.get("deferred").is_none(), "{row}");
    }
    assert_eq!(rows[0]["dstHost"], "second.example.com");
    let id = rows[0]["id"].as_str().unwrap();
    assert!(
        id.starts_with(&format!("event-{}-", 1_800_000_002 * S)),
        "{id}"
    );

    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .unwrap();
    let hits = next_action(&mut ws, "ruleHits").await;
    assert_eq!(hits["lossy"], false, "{hits}");
    assert_eq!(hits["lastGapUnixMs"], Value::Null, "{hits}");

    bridge.shutdown();
}
