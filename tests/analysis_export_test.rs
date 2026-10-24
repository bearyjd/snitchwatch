//! Production authenticated WS + mock-daemon acceptance. GUI preview/save is
//! verified separately by Kirigami's actual-controller probe; this crate stays Qt-free.
use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_bridge::bridge_capabilities::AI_CACHE_SUMMARY_EXPORT_V1;
use snitchwatch_bridge_cli::{run, BridgeConfig, RunningBridge};
use snitchwatch_proto::protocol::{
    ClientConfig, Connection, Event, Notification, NotificationReply, Rule, Statistics,
};
use std::{
    io::Write,
    ops::Deref,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{net::UnixStream, sync::mpsc};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tracing_subscriber::util::SubscriberInitExt;

type Ws = WebSocketStream<UnixStream>;
const APP: &str = "/usr/bin/PRIVATE-SELECTOR-DIRECTORY/allowed-雪";
const DISCLOSED_BASENAME: &str = "allowed-雪";
const INVALID_SELECTOR_CANARY: &str = "SECRETSELECTOR-CANARY-";
const HOST: &str = "PRIVATE-HOST-CANARY.invalid";
const IP: &str = "192.0.2.193";
const RULE: &str = "PRIVATE-RULE-CANARY";
const FRAME_LIMIT: usize = 256;

#[derive(Default)]
struct Trace {
    bytes: Vec<u8>,
    clipped: bool,
}
#[derive(Clone)]
struct TraceWriter(Arc<Mutex<Trace>>);
impl Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut trace = self.0.lock().unwrap();
        if trace.bytes.len().saturating_add(bytes.len()) <= 4 * 1024 * 1024 {
            trace.bytes.extend_from_slice(bytes);
        } else {
            trace.clipped = true;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn traces() -> Arc<Mutex<Trace>> {
    static TRACE: OnceLock<Arc<Mutex<Trace>>> = OnceLock::new();
    TRACE
        .get_or_init(|| {
            let trace = Arc::new(Mutex::new(Trace::default()));
            let writer = TraceWriter(trace.clone());
            tracing_subscriber::fmt()
                .with_ansi(false)
                // Dependency transport TRACE logs raw live WS traffic. This proves
                // application logging privacy; dependency logging has a separate gate.
                .with_env_filter("off,snitchwatch_bridge=trace,snitchwatch_bridge_cli=trace")
                .with_writer(move || writer.clone())
                .finish()
                .init();
            trace
        })
        .clone()
}
struct BridgeGuard(Option<RunningBridge>);
impl Deref for BridgeGuard {
    type Target = RunningBridge;
    fn deref(&self) -> &RunningBridge {
        self.0.as_ref().unwrap()
    }
}
impl Drop for BridgeGuard {
    fn drop(&mut self) {
        if let Some(bridge) = self.0.take() {
            bridge.shutdown();
        }
    }
}
struct Fixture {
    bridge: BridgeGuard,
    daemon: MockOpensnitchd,
    replies: Option<mpsc::Sender<NotificationReply>>,
    notifications: mpsc::Receiver<Notification>,
    directory: tempfile::TempDir,
    runtime_entries: Vec<std::ffi::OsString>,
}
impl Fixture {
    async fn start(capacity: usize) -> Self {
        let _ = traces();
        let directory = tempfile::tempdir().unwrap();
        let bridge = BridgeGuard(Some(
            run(BridgeConfig {
                grpc_bind: "127.0.0.1:0".parse().unwrap(),
                ws_socket_path: directory.path().join("bridge.sock"),
                cache_capacity: capacity,
            })
            .await
            .expect("mock bridge startup failed"),
        ));
        let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
            .await
            .unwrap();
        daemon
            .subscribe_with_config(ClientConfig {
                name: "analysis-export-mock".into(),
                is_firewall_running: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let (replies, notifications) = daemon.open_notifications().await.unwrap();
        let mut ready = bridge.daemon_stream_ready();
        tokio::time::timeout(
            Duration::from_secs(3),
            ready.wait_for(|generation| *generation >= 1),
        )
        .await
        .unwrap()
        .unwrap();
        let runtime_entries = directory_entries(directory.path());
        Self {
            bridge,
            daemon,
            replies: Some(replies),
            notifications,
            directory,
            runtime_entries,
        }
    }
    async fn connect(&self) -> Ws {
        let stream = UnixStream::connect(&self.bridge.ws_socket_path)
            .await
            .unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
            .await
            .unwrap();
        ws.send(Message::Text(self.bridge.ws_token.as_str().into()))
            .await
            .unwrap();
        let (ack, _) = frame(&mut ws).await;
        assert_eq!(ack["action"], "authenticated");
        assert!(
            ack["capabilities"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|cap| cap == AI_CACHE_SUMMARY_EXPORT_V1)),
            "authenticated bridge must advertise the functioning cache export service"
        );
        ws
    }
    async fn populate(&mut self, events: Vec<Event>) {
        let count = events.len() as u64;
        self.daemon
            .ping_with_stats(
                1,
                Statistics {
                    uptime: 100,
                    rule_hits: count,
                    events,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    fn no_export_files(&self) {
        assert!(
            directory_entries(self.directory.path()) == self.runtime_entries,
            "mock runtime file set changed without Save"
        );
    }
    async fn finish(mut self) {
        self.no_export_files();
        drop(self.replies.take());
        if let Some(bridge) = self.bridge.0.take() {
            bridge.shutdown();
        }
        let mut commands = 0usize;
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.notifications.recv().await.is_some() {
                commands += 1;
            }
        })
        .await
        .expect("mock notification stream did not close");
        assert_eq!(commands, 0, "cache export must send no daemon commands");
    }
}
fn directory_entries(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut entries: Vec<_> = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    entries
}
async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string())).await.unwrap();
}
async fn frame(ws: &mut Ws) -> (Value, usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return (
                        serde_json::from_str(&text).expect("server JSON invalid"),
                        text.len(),
                    )
                }
                Some(Ok(_)) => {}
                _ => panic!("mock websocket closed before expected frame"),
            }
        }
    })
    .await
    .expect("mock websocket made no progress")
}
fn is_export(value: &Value) -> bool {
    matches!(
        value["action"].as_str(),
        Some("cacheSummaryExport" | "cacheSummaryExportRefused")
    )
}
async fn export(ws: &mut Ws, id: &str, filters: Value) -> (String, Value, usize) {
    send(ws, json!({"action":"requestCacheSummaryExport","requestId":id,"schemaVersion":1,"filters":filters})).await;
    for _ in 0..FRAME_LIMIT {
        let (value, bytes) = frame(ws).await;
        if is_export(&value) {
            assert_eq!(value["requestId"], id);
            assert_eq!(
                value["action"], "cacheSummaryExport",
                "valid mock export must succeed"
            );
            assert!(bytes <= 1_052_658 && bytes + 14 <= 1_052_672);
            let artifact = value["artifact"]
                .as_str()
                .expect("artifact must be UTF-8 JSON")
                .to_owned();
            assert!(artifact.len() <= 1_048_576);
            let document = serde_json::from_str(&artifact).expect("artifact JSON invalid");
            return (artifact, document, bytes);
        }
    }
    panic!("no export within bounded fixture frames");
}
async fn refusal(ws: &mut Ws, id: &str, schema: u64, filters: Value) -> Value {
    send(ws, json!({"action":"requestCacheSummaryExport","requestId":id,"schemaVersion":schema,"filters":filters})).await;
    for _ in 0..FRAME_LIMIT {
        let (value, _) = frame(ws).await;
        if is_export(&value) {
            assert_eq!(value["action"], "cacheSummaryExportRefused");
            assert_eq!(value["requestId"], id);
            assert!(value.get("artifact").is_none());
            return value;
        }
    }
    panic!("no refusal within bounded fixture frames");
}
/// Check the ordered broadcast window and drain both clients' snapshots.
/// This does not order ReplyTo/session slots: forward_outbound selects those
/// independently. Deterministic session-slot isolation and WS stamping
/// regressions must supply the companion private-queue isolation evidence.
async fn broadcast_delivery_barrier(a: &mut Ws, b: &mut Ws) {
    send(b, json!({"action":"requestSnapshot"})).await;
    for ws in [a, b] {
        let mut saw_clear = false;
        let mut saw_end = false;
        for _ in 0..FRAME_LIMIT {
            let (value, _) = frame(ws).await;
            assert!(
                !is_export(&value),
                "unexpected export while observing the broadcast snapshot"
            );
            if value["action"] == "clearConnectionRows" {
                saw_clear = true;
            }
            if saw_clear && value["action"] == "filterPauseState" {
                saw_end = true;
                break;
            }
        }
        assert!(saw_end, "no complete snapshot broadcast barrier");
    }
}
fn event(path: &str, host: &str, action: &str, ms: i64) -> Event {
    Event {
        connection: Some(Connection {
            process_path: path.into(),
            dst_host: host.into(),
            dst_ip: IP.into(),
            dst_port: 443,
            protocol: "tcp".into(),
            ..Default::default()
        }),
        rule: Some(Rule {
            name: RULE.into(),
            action: action.into(),
            duration: "always".into(),
            enabled: true,
            ..Default::default()
        }),
        unixnano: ms * 1_000_000,
        ..Default::default()
    }
}
fn selection() -> Value {
    json!({"window":{"fromMs":1000,"toMs":2000},"app":{"kind":"path","value":APP},"verdicts":["allow"]})
}
fn nine_events() -> Vec<Event> {
    vec![
        event(APP, HOST, "allow", 999),
        event(APP, HOST, "allow", 1000),
        event(APP, HOST, "allow", 1001),
        event(APP, HOST, "deny", 1500),
        event("/usr/bin/other", HOST, "allow", 1999),
        event(APP, HOST, "allow", 2000),
        event("", "PRIVATE-MISSING-HOST.invalid", "allow", 1600),
        event(&"X".repeat(4097), HOST, "allow", 1700),
        event(APP, HOST, "allow", -1),
    ]
}
fn assert_artifact_private(text: &str) {
    for forbidden in [
        "PRIVATE-SELECTOR-DIRECTORY",
        APP,
        HOST,
        IP,
        RULE,
        "PRIVATE-MISSING-HOST.invalid",
        INVALID_SELECTOR_CANARY,
    ] {
        assert!(
            !text.contains(forbidden),
            "export evidence leaked a private source canary"
        );
    }
}
// A disclosed basename is legitimate artifact data, but no observation
// metadata or selector belongs in a refusal, diagnostic report or app trace.
fn assert_diagnostic_private(text: &str) {
    assert_artifact_private(text);
    assert!(
        !text.contains(DISCLOSED_BASENAME),
        "diagnostic evidence leaked a disclosed basename"
    );
}
fn assert_base_counts(document: &Value, selected: usize, groups: usize) {
    assert_eq!(
        document["counts"],
        json!({"retained":9,"inspected":9,"acceptedSnapshot":7,"selected":selected,"emitted":selected,"emittedGroups":groups})
    );
    assert_eq!(
        document["coverage"]["rowOmissions"],
        json!({"inspectionLimit":0,"fieldTooLong":1,"inputBudget":0,"invalidTimestamp":1})
    );
    assert_eq!(document["coverage"]["unexaminedRetained"], 0);
    assert_eq!(document["coverage"]["completeness"], "incomplete");
    for reason in ["groupLimit", "artifactLimit", "wireLimit"] {
        assert_eq!(
            document["coverage"]["groupOmissions"][reason],
            json!({"groups":0,"observations":0})
        );
    }
}

// This real-WS scenario checks positive A/B delivery and broadcast leaks.
// Compose it with deterministic private-slot isolation/stamping regressions:
// a completed snapshot does not prove independent private queues are empty.
#[tokio::test]
async fn two_clients_receive_only_their_export() {
    let mut fixture = Fixture::start(64).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    fixture.populate(nine_events()).await;
    let (_, first, _) = export(&mut a, "a-first", selection()).await;
    assert_base_counts(&first, 2, 1);
    broadcast_delivery_barrier(&mut a, &mut b).await;
    let refused = refusal(&mut a, "a-version", 2, json!({})).await;
    assert_eq!(refused["reason"], "unsupportedVersion");
    broadcast_delivery_barrier(&mut a, &mut b).await;
    let refused = refusal(&mut a, "a-filter", 1, json!({"mode":"raw"})).await;
    assert_eq!(refused["reason"], "invalidRequest");
    broadcast_delivery_barrier(&mut a, &mut b).await;
    let (_, second, _) = export(&mut a, "a-second", selection()).await;
    assert_base_counts(&second, 2, 1);
    for field in ["appRef", "destinationRef"] {
        assert_ne!(first["groups"][0][field], second["groups"][0][field]);
    }
    broadcast_delivery_barrier(&mut a, &mut b).await;
    let (_, own, _) = export(&mut b, "b-own", selection()).await;
    assert_base_counts(&own, 2, 1);
    broadcast_delivery_barrier(&mut a, &mut b).await;
    fixture.finish().await;
}

#[tokio::test]
async fn cache_export_end_to_end_privacy() {
    let mut fixture = Fixture::start(64).await;
    let mut ws = fixture.connect().await;
    fixture.populate(nine_events()).await;
    let (artifact, document, _) = export(&mut ws, "privacy-selected", selection()).await;
    assert_artifact_private(&artifact);
    assert!(
        artifact.contains(DISCLOSED_BASENAME),
        "permitted basename should be disclosed"
    );
    assert_base_counts(&document, 2, 1);
    assert_eq!(document["groups"][0]["count"], 2);
    assert_eq!(document["groups"][0]["firstObservedAtMs"], 1000);
    assert_eq!(document["groups"][0]["lastObservedAtMs"], 1001);
    assert_eq!(document["groups"][0]["verdict"], "allow");
    let (artifact, document, _) = export(
        &mut ws,
        "privacy-missing",
        json!({"app":{"kind":"missingPath"}}),
    )
    .await;
    assert_artifact_private(&artifact);
    assert_base_counts(&document, 1, 1);
    assert!(document["groups"][0].get("appBasename").is_none());
    let (_, document, _) = export(&mut ws, "privacy-empty", json!({"verdicts":[]})).await;
    assert_base_counts(&document, 0, 0);
    let (artifact, document, _) = export(&mut ws, "privacy-all", json!({})).await;
    assert_artifact_private(&artifact);
    assert_base_counts(&document, 7, 4);
    let invalid_selector = format!(
        "{INVALID_SELECTOR_CANARY}{}",
        "x".repeat(4097 - INVALID_SELECTOR_CANARY.len())
    );
    assert_eq!(invalid_selector.len(), 4097);
    let refused = refusal(
        &mut ws,
        "privacy-refused",
        1,
        json!({"app":{"kind":"path","value":invalid_selector}}),
    )
    .await;
    assert_eq!(refused["reason"], "invalidRequest");
    assert_diagnostic_private(&refused.to_string());
    send(&mut ws, json!({"action":"recheckDiagnostics"})).await;
    let mut saw_diagnostics = false;
    for _ in 0..FRAME_LIMIT {
        let (value, _) = frame(&mut ws).await;
        if value["action"] == "diagnosticsReport" {
            assert_diagnostic_private(&value.to_string());
            saw_diagnostics = true;
            break;
        }
    }
    assert!(saw_diagnostics);
    fixture.no_export_files();
    fixture.finish().await;
    let trace = traces();
    let trace = trace.lock().unwrap();
    assert!(!trace.clipped, "trace evidence exceeded its bound");
    assert_diagnostic_private(&String::from_utf8_lossy(&trace.bytes));
}

#[tokio::test]
async fn bounded_whole_group_omissions_over_real_ws() {
    let mut fixture = Fixture::start(2048).await;
    let mut ws = fixture.connect().await;
    let path = format!("/usr/bin/PRIVATE-SELECTOR-DIRECTORY/{}", "\"".repeat(300));
    let events = (0..2000)
        .map(|index| {
            event(
                &path,
                &format!("PRIVATE-HOST-{index}.invalid"),
                "allow",
                1000,
            )
        })
        .collect();
    fixture.populate(events).await;
    let (artifact, document, _) = export(&mut ws, "bounded", json!({})).await;
    assert!(!artifact.contains("PRIVATE-SELECTOR-DIRECTORY"));
    assert!(!artifact.contains("PRIVATE-HOST-"));
    assert_eq!(
        document["counts"],
        json!({"retained":2000,"inspected":2000,"acceptedSnapshot":2000,"selected":2000,"emitted":500,"emittedGroups":500})
    );
    assert_eq!(
        document["coverage"]["groupOmissions"],
        json!({"groupLimit":{"groups":0,"observations":0},"artifactLimit":{"groups":1000,"observations":1000},"wireLimit":{"groups":500,"observations":500}})
    );
    assert_eq!(document["groups"].as_array().unwrap().len(), 500);
    fixture.finish().await;
}
