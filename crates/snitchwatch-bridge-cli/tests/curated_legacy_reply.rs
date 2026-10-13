//! Issue #129: a legacy `setCuratedDefaults` request that only produced
//! refusals (consent required, unknown id) is answered at once, not when
//! some unrelated event next wakes the curated worker.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge_cli::BridgeMode;
use tokio_tungstenite::tungstenite::Message;

mod curated_support;
use curated_support::{connect_daemon, nothing_sent, start};

/// A quiet connection: no daemon traffic happens after `ids` are sent.
async fn legacy_reply(ids: &[&str]) -> serde_json::Value {
    let s = start(BridgeMode::System).await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    nothing_sent(&mut seen).await;
    let transport = tokio::net::UnixStream::connect(&s.bridge.ws_socket_path)
        .await
        .unwrap();
    let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", transport)
        .await
        .unwrap();
    gui.send(Message::Text(s.bridge.ws_token.as_str().into()))
        .await
        .unwrap();
    let ack = gui.next().await.unwrap().unwrap();
    assert!(ack.to_string().contains("authenticated"));
    let request = serde_json::json!({
        "action": "setCuratedDefaults", "requestId": "legacy-1", "ids": ids, "on": true,
    });
    gui.send(Message::Text(request.to_string())).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Message::Text(text) = gui.next().await.unwrap().unwrap() {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                if value["action"] == "curatedDefaultsRequestResult"
                    && value["requestId"] == "legacy-1"
                {
                    break value;
                }
            }
        }
    })
    .await
    .expect("no curatedDefaultsRequestResult within 2 s on a quiet connection");
    s.bridge.shutdown();
    reply
}

#[tokio::test]
async fn a_refused_legacy_dns_request_is_answered_without_any_other_event() {
    let reply = legacy_reply(&["dns-resolved"]).await;
    assert_eq!(reply["results"][0]["outcome"], "refusedConsent", "{reply}");
}

#[tokio::test]
async fn an_unknown_id_only_legacy_request_is_answered_without_any_other_event() {
    let reply = legacy_reply(&["no-such-entry"]).await;
    assert_eq!(reply["results"][0]["outcome"], "unknownId", "{reply}");
}
