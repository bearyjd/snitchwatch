//! P2.6 Part 1, end to end below QML: a mock opensnitchd subscribes with two
//! rules and pings with matched events; the bridge's `RuleHits` crosses the
//! real WebSocket, is routed with the shell's own predicate
//! (`interests_rules`), JSON-encoded the way the feed does, and folded into
//! the `RuleHitsView` the Rules tab reads. No Qt objects are instantiated, so
//! it runs fully headless.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_kirigami::bridge_dispatch::{encode_server, interests_rules};
use snitchwatch_kirigami::rules::hits::{RowHits, RuleHitsView};
use snitchwatch_kirigami::rules::row_store::Rule;
use snitchwatch_proto::protocol::{ClientConfig, Event, Operator, Rule as DaemonRule, Statistics};
use tokio::net::UnixStream;
use tokio_tungstenite::{client_async, tungstenite::Message};

fn daemon_rule(name: &str) -> DaemonRule {
    DaemonRule {
        name: name.into(),
        enabled: true,
        action: "allow".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn event(rule_name: &str, unixnano: i64) -> Event {
    Event {
        rule: Some(DaemonRule {
            name: rule_name.into(),
            ..Default::default()
        }),
        unixnano,
        ..Default::default()
    }
}

fn row(name: &str, nolog: bool) -> Rule {
    Rule {
        name: name.into(),
        nolog,
        ..Default::default()
    }
}

#[tokio::test]
async fn rule_hits_cross_the_websocket_and_reach_the_rules_tab_view() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg.clone()).await.expect("bridge run failed");

    // The daemon connects, subscribes with two rules and says HELLO.
    let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    mock.subscribe_with_config(ClientConfig {
        name: "mock".into(),
        rules: vec![daemon_rule("allow-curl"), daemon_rule("allow-quiet")],
        ..Default::default()
    })
    .await
    .unwrap();
    let (_replies, _inbound) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(10), ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    mock.ping_with_stats(
        1,
        Statistics {
            uptime: 10,
            rule_hits: 3,
            events: vec![
                event("allow-curl", 1_700_000_000_000_000_000),
                event("allow-curl", 1_700_000_005_000_000_000),
                event("allow-quiet", 1_700_000_001_000_000_000),
            ],
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // A GUI session: authenticate, then ask for the snapshot.
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

    let mut view = RuleHitsView::default();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msg: ServerMessage = match ws.next().await.expect("closed").expect("ws error") {
                Message::Text(text) => serde_json::from_str(&text).expect("decode"),
                _ => continue,
            };
            if !interests_rules(&msg) {
                continue;
            }
            // What the feed hands the model: routed, then JSON.
            let json = encode_server(&msg).expect("encode");
            let typed: ServerMessage = serde_json::from_str(&json).expect("round trip");
            if view.apply(&typed) {
                return;
            }
        }
    })
    .await
    .expect("no RuleHits reached the Rules tab");

    assert_eq!(
        view.for_rule(&row("allow-curl", false)),
        RowHits::Counted {
            count: 2,
            last_hit_unix_ms: 1_700_000_005_000
        }
    );
    assert_eq!(
        view.for_rule(&row("allow-never", false)),
        RowHits::Counted {
            count: 0,
            last_hit_unix_ms: 0
        }
    );
    // The same name on a `nolog` rule is "not counted", whatever was seen.
    assert_eq!(
        view.for_rule(&row("allow-quiet", true)),
        RowHits::NotCounted
    );
    let info: serde_json::Value = serde_json::from_str(&view.info_json()).unwrap();
    assert_eq!(info["available"], true);
    assert_eq!(info["counting"], true);
    assert_eq!(
        info["persistent"], false,
        "an in-process bridge saves nothing"
    );

    bridge.shutdown();
}
