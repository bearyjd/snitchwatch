//! Rule export/import end to end on the system (Unix) transport, the only
//! one import runs on: `MockOpensnitchd` ↔ a real bridge ↔ WebSocket GUIs.
//! What reaches the daemon is checked on the mock's notification stream:
//! `CHANGE_RULE` only, one rule each. (`tests/rules_io_test.rs` covers the
//! refusal on the legacy TCP transport.)
//!
//! The daemon's socket accepts any peer here: `RootUnixIncoming` would
//! refuse a test that doesn't run as root.

use crate::{run_with_incoming, BridgeConfig, GrpcEndpoint, RunOptions, RunningBridge};
use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_proto::protocol::{
    Action, ClientConfig, Notification, NotificationReply, NotificationReplyCode, Operator, Rule,
};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<UnixStream>;

/// A daemon socket that accepts every peer (tests only).
struct AnyPeer(UnixListener);

impl tokio_stream::Stream for AnyPeer {
    type Item = std::io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0
            .poll_accept(cx)
            .map(|accepted| Some(accepted.map(|(stream, _)| stream)))
    }
}

async fn connect_ws(bridge: &RunningBridge) -> Ws {
    let stream = UnixStream::connect(&bridge.ws_socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(bridge.ws_token.as_str().to_string()))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["action"], "authenticated");
    ws
}

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

async fn send(ws: &mut Ws, message: Value) {
    ws.send(Message::Text(message.to_string())).await.unwrap();
}

fn host_rule(name: &str, data: &str) -> Rule {
    Rule {
        created: 1_800_000_000,
        name: name.into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: data.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn file_rule(name: &str) -> Value {
    json!({
        "name": name, "enabled": true, "action": "deny", "duration": "always",
        "operator": { "type": "simple", "operand": "dest.host", "data": format!("{name}.example") },
    })
}

fn document(rules: Vec<Value>) -> Value {
    json!({ "format": "snitchwatch.rules", "version": 1, "rules": rules })
}

struct Setup {
    bridge: RunningBridge,
    ws: Ws,
    replies: mpsc::Sender<NotificationReply>,
    notifications: mpsc::Receiver<Notification>,
    _dir: tempfile::TempDir,
}

/// A bridge on the Unix transport whose daemon subscribed with `rules` and
/// said HELLO, and a GUI.
async fn setup(rules: Vec<Rule>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let grpc_path = dir.path().join("opensnitchd.sock");
    let bridge = run_with_incoming(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        GrpcEndpoint::Unix(grpc_path.clone()),
        AnyPeer(UnixListener::bind(&grpc_path).unwrap()),
        None,
        None,
        RunOptions::in_process(),
    )
    .await
    .unwrap();
    let mut ws = connect_ws(&bridge).await;
    let channel = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = grpc_path.clone();
            async move {
                UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap();
    let mut mock = MockOpensnitchd::from_channel(channel);
    // Before the daemon's HELLO there is nothing to export.
    send(
        &mut ws,
        json!({ "action": "exportRules", "requestId": "e0" }),
    )
    .await;
    let unavailable = next_action(&mut ws, "rulesExportUnavailable").await;
    assert_eq!(unavailable["requestId"], "e0");
    assert!(unavailable["reason"].as_str().unwrap().contains("loaded"));

    mock.subscribe_with_config(ClientConfig {
        name: "mock".into(),
        rules,
        ..Default::default()
    })
    .await
    .unwrap();
    let (replies, notifications) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .unwrap()
        .unwrap();
    next_action(&mut ws, "setRules").await;
    Setup {
        bridge,
        ws,
        replies,
        notifications,
        _dir: dir,
    }
}

fn reply(id: u64, ok: bool, data: &str) -> NotificationReply {
    NotificationReply {
        id,
        code: if ok {
            NotificationReplyCode::Ok
        } else {
            NotificationReplyCode::Error
        } as i32,
        data: data.into(),
    }
}

async fn preview(ws: &mut Ws, rules: Vec<Value>) -> (String, Vec<Value>) {
    let request =
        json!({ "action": "previewRulesImport", "requestId": "p1", "document": document(rules) });
    send(ws, request).await;
    let preview = next_action(ws, "rulesImportPreview").await;
    assert_eq!(preview["requestId"], "p1");
    (
        preview["previewId"].as_str().unwrap().to_string(),
        preview["items"].as_array().unwrap().clone(),
    )
}

#[tokio::test]
async fn export_lists_the_daemon_rules_in_name_order_to_the_asking_gui_only() {
    let mut s = setup(vec![
        host_rule("c-third", "c.example"),
        host_rule("a-first", "a.example"),
        host_rule("b-second", "b.example"),
    ])
    .await;
    let mut other = connect_ws(&s.bridge).await;
    send(
        &mut s.ws,
        json!({ "action": "exportRules", "requestId": "e1" }),
    )
    .await;
    let export = next_action(&mut s.ws, "rulesExport").await;
    assert_eq!(export["requestId"], "e1");
    let names: Vec<_> = export["document"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["a-first", "b-second", "c-third"]);
    assert!(!export.to_string().contains("displayName"));

    // The other GUI sees nothing of it.
    send(&mut other, json!({ "action": "requestSnapshot" })).await;
    loop {
        let v = next_json(&mut other).await;
        assert_ne!(v["action"], "rulesExport", "sent to a GUI that didn't ask");
        if v["action"] == "trayState" {
            break;
        }
    }
    s.bridge.shutdown();
}

#[tokio::test]
async fn apply_sends_one_change_rule_per_ticked_rule_publishes_once_and_never_deletes() {
    // "z-cached" is not in the file: import must leave it alone.
    let mut s = setup(vec![host_rule("z-cached", "z.example")]).await;
    let (id, items) = preview(
        &mut s.ws,
        vec![
            file_rule("b-ok"),
            file_rule("c-bad"),
            file_rule("d-ok"),
            file_rule("e-unticked"),
        ],
    )
    .await;
    assert_eq!(items.len(), 4);
    assert!(items.iter().all(|i| i["kind"] == "add"), "{items:?}");

    let apply = json!({ "action": "applyRulesImport", "requestId": "a1", "previewId": id,
                        "include": ["d-ok", "c-bad", "b-ok"] });
    send(&mut s.ws, apply).await;
    let mut seen = Vec::new();
    for _ in 0..3 {
        let n = tokio::time::timeout(Duration::from_secs(5), s.notifications.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n.r#type, Action::ChangeRule as i32, "only CHANGE_RULE");
        assert_eq!(n.rules.len(), 1, "one rule per notification");
        let name = n.rules[0].name.clone();
        let ok = name != "c-bad";
        let data = if ok { "" } else { "bad regexp" };
        s.replies.send(reply(n.id, ok, data)).await.unwrap();
        seen.push(name);
    }
    assert_eq!(seen, vec!["b-ok", "c-bad", "d-ok"]);

    let mut rejected = None;
    let mut set_rules = Vec::new();
    let mut result = None;
    // Read on a little past the result: the list and the result travel on
    // different channels to the GUI.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let wait = if result.is_some() { 300 } else { 10_000 };
        let Ok(Some(Ok(Message::Text(t)))) =
            tokio::time::timeout(Duration::from_millis(wait), s.ws.next()).await
        else {
            break;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        match v["action"].as_str().unwrap() {
            "rulesImportProgress" if v["name"] == "c-bad" => rejected = Some(v["outcome"].clone()),
            "setRules" => set_rules.push(v["rules"].clone()),
            "rulesImportResult" => result = Some(v),
            _ => {}
        }
    }
    let result = result.expect("no result");
    assert_eq!(result["previewId"], id.as_str());
    assert_eq!(
        (result["applied"].as_u64(), result["rejected"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(
        rejected.unwrap(),
        json!({ "status": "rejected", "reason": "bad regexp" })
    );
    assert_eq!(set_rules.len(), 1, "the list is published once per apply");
    let names: Vec<_> = set_rules[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["b-ok", "d-ok", "z-cached"]);
    assert!(
        s.notifications.try_recv().is_err(),
        "nothing else reached the daemon (no DELETE_RULE, no unticked rule)"
    );
    s.bridge.shutdown();
}

#[tokio::test]
async fn a_toggle_between_preview_and_apply_refuses_the_apply() {
    let mut s = setup(vec![host_rule("a-cached", "a.example")]).await;
    let (id, _) = preview(&mut s.ws, vec![file_rule("b-new")]).await;

    let mut toggled = file_rule("a-cached");
    toggled["operator"]["data"] = json!("a.example");
    toggled["enabled"] = json!(false);
    let update = json!({ "action": "updateRule", "ruleId": "a-cached", "rule": toggled });
    send(&mut s.ws, update).await;
    let n = s.notifications.recv().await.unwrap();
    s.replies.send(reply(n.id, true, "")).await.unwrap();
    next_action(&mut s.ws, "setRules").await;

    let apply = json!({ "action": "applyRulesImport", "requestId": "a2", "previewId": id,
                        "include": ["b-new"] });
    send(&mut s.ws, apply).await;
    let refused = next_action(&mut s.ws, "rulesImportRefused").await;
    assert_eq!(refused["requestId"], "a2");
    let reason = refused["reason"].as_str().unwrap();
    assert!(reason.contains("changed since the preview"), "{reason}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.notifications.try_recv().is_err(), "nothing sent");
    s.bridge.shutdown();
}

/// 200 rules against a daemon that answers each after 50 ms: never more than
/// 8 await an answer, and a GUI reading throughout gets every outcome.
#[tokio::test]
async fn a_large_import_paces_itself() {
    let mut s = setup(Vec::new()).await;
    let rules: Vec<_> = (0..200).map(|i| file_rule(&format!("r{i:03}"))).collect();
    let (id, items) = preview(&mut s.ws, rules).await;
    let include: Vec<_> = items.iter().map(|i| i["name"].clone()).collect();

    let outstanding = Arc::new(AtomicUsize::new(0));
    let max_outstanding = Arc::new(AtomicUsize::new(0));
    let (replies, mut notifications) = (s.replies.clone(), s.notifications);
    let (out, max) = (outstanding.clone(), max_outstanding.clone());
    tokio::spawn(async move {
        while let Some(n) = notifications.recv().await {
            assert_eq!(n.r#type, Action::ChangeRule as i32);
            let now = out.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(now, Ordering::SeqCst);
            let (replies, out) = (replies.clone(), out.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                out.fetch_sub(1, Ordering::SeqCst);
                let _ = replies.send(reply(n.id, true, "")).await;
            });
        }
    });

    let apply = json!({ "action": "applyRulesImport", "requestId": "a3", "previewId": id,
                        "include": include });
    send(&mut s.ws, apply).await;
    let mut progress = 0;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let v = next_json(&mut s.ws).await;
            match v["action"].as_str().unwrap() {
                "rulesImportProgress" => progress += 1,
                "rulesImportResult" => return v,
                _ => {}
            }
        }
    })
    .await
    .expect("no result");
    assert_eq!(result["applied"], 200, "{result}");
    assert_eq!(progress, 200);
    let max = max_outstanding.load(Ordering::SeqCst);
    assert!((2..=8).contains(&max), "max in flight {max}");
    s.bridge.shutdown();
}
