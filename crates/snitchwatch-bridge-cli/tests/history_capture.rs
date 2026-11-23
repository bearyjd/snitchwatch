//! An enabled bridge records daemon events and prompt outcomes through real
//! gRPC and WebSocket connections. A blocked writer or a full queue never
//! delays a prompt's verdict or a ping's reply.
//!
//! Its own test binary: `history_off.rs` asserts process-wide facts (no writer
//! thread, no queue) that an enabled bridge here would break.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::{MockError, MockOpensnitchd};
use serde_json::{json, Value};
use snitchwatch_bridge::history::{enable, locate, Consent, Mode};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{ClientConfig, Connection, Event, Operator, Rule, Statistics};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Far above what a verdict takes (milliseconds), far below the writer's
/// busy wait: a verdict that waited for the writer would miss it.
const VERDICT_BOUND: Duration = Duration::from_millis(1200);

fn state_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("history-capture-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

struct Boot {
    bridge: RunningBridge,
    ws: WebSocketStream<UnixStream>,
    daemon: MockOpensnitchd,
    grpc_path: PathBuf,
    db: PathBuf,
    _sockets: tempfile::TempDir,
    _state: tempfile::TempDir,
}

async fn next_frame(ws: &mut WebSocketStream<UnixStream>) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = ws.next().await {
            match frame.unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Close(_) => break,
                _ => {}
            }
        }
        panic!("WebSocket closed before the expected frame");
    })
    .await
    .expect("no frame")
}

async fn frame_with(ws: &mut WebSocketStream<UnixStream>, action: &str) -> Value {
    loop {
        let frame = next_frame(ws).await;
        if frame["action"] == action {
            return frame;
        }
    }
}

/// A bridge with history enabled in its state directory, a GUI that has
/// authenticated and a subscribed daemon.
async fn boot_enabled() -> Boot {
    let state = state_tempdir();
    let root = state.path().canonicalize().unwrap();
    let location = locate(Mode::User, Some(&root)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "test".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            cache_capacity: 4096,
            ..BridgeConfig::user_unix_for_tests(sockets.path())
        },
        RunOptions {
            storage: Storage::Persistent(root),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
    )
    .await
    .unwrap();
    let stream = UnixStream::connect(&bridge.ws_socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(bridge.ws_token.as_str().to_string()))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut ws).await["action"], "authenticated");
    let grpc_path = bridge.grpc_endpoint.unix_path().unwrap().to_path_buf();
    let mut daemon = MockOpensnitchd::connect_unix(&grpc_path).await.unwrap();
    daemon
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            config: r#"{"DefaultAction": "deny"}"#.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    Boot {
        db: location.db_path(),
        bridge,
        ws,
        daemon,
        grpc_path,
        _sockets: sockets,
        _state: state,
    }
}

fn event(process: &str, host: &str, rule_name: &str) -> Event {
    with_operand(process, host, rule_name, process)
}

fn with_operand(process: &str, host: &str, rule_name: &str, operand: &str) -> Event {
    Event {
        connection: Some(Connection {
            protocol: "tcp".into(),
            dst_host: host.into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            process_path: process.into(),
            process_args: vec!["curl".into(), "ARGSCANARY".into()],
            ..Default::default()
        }),
        rule: Some(Rule {
            name: rule_name.into(),
            action: "allow".into(),
            duration: "always".into(),
            enabled: true,
            operator: Some(Operator {
                r#type: "simple".into(),
                operand: "process.path".into(),
                data: operand.into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        unixnano: 1_700_000_000_000_000_000,
        ..Default::default()
    }
}

async fn ping(daemon: &mut MockOpensnitchd, events: Vec<Event>) -> Duration {
    let started = Instant::now();
    daemon
        .ping_with_stats(
            1,
            Statistics {
                events,
                uptime: 5,
                rule_hits: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    started.elapsed()
}

fn row_count(db: &Path) -> i64 {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.busy_timeout(Duration::from_millis(50)).unwrap();
    conn.query_row(
        "SELECT coalesce(sum(repeat_count), 0) FROM events",
        [],
        |r| r.get(0),
    )
    .unwrap_or(-1)
}

async fn rows_reach(db: &Path, at_least: i64) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let n = row_count(db);
        if n >= at_least || Instant::now() > deadline {
            return n;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn spawn_ask(boot: &Boot) -> JoinHandle<Result<Rule, MockError>> {
    let grpc_path = boot.grpc_path.clone();
    tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect_unix(&grpc_path).await.unwrap();
        daemon
            .ask_rule(Connection {
                protocol: "tcp".into(),
                src_ip: "192.0.2.1".into(),
                src_port: 49321,
                dst_host: "updates.example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/prompted".into(),
                ..Default::default()
            })
            .await
    })
}

async fn pending_row_id(ws: &mut WebSocketStream<UnixStream>) -> String {
    // The pings' decided rows were broadcast too; the prompt's row has no
    // action yet.
    loop {
        let inserted = frame_with(ws, "insertConnectionRows").await;
        let row = &inserted["rows"][0];
        if row["action"] == Value::Null {
            assert_eq!(row["srcIp"], "192.0.2.1");
            assert_eq!(row["srcPort"], 49321);
            return row["id"].as_str().unwrap().to_string();
        }
    }
}

async fn send_verdict(ws: &mut WebSocketStream<UnixStream>, row_id: &str, verdict: &str) {
    ws.send(Message::Text(
        json!({
            "action": "setVerdict",
            "rowId": row_id,
            "verdict": verdict,
            "scope": "this_host",
            "duration": "always",
        })
        .to_string(),
    ))
    .await
    .unwrap();
}

async fn await_ask(ask: JoinHandle<Result<Rule, MockError>>) -> Result<Rule, Box<MockError>> {
    tokio::time::timeout(Duration::from_secs(10), ask)
        .await
        .expect("the prompt was never answered")
        .unwrap()
        .map_err(Box::new)
}

/// How long an AskRule answer took from the GUI's `setVerdict`.
async fn verdict_latency(boot: &mut Boot) -> Duration {
    let ask = spawn_ask(boot);
    let row_id = pending_row_id(&mut boot.ws).await;
    let started = Instant::now();
    send_verdict(&mut boot.ws, &row_id, "allow").await;
    let rule = await_ask(ask).await.unwrap();
    assert_eq!(rule.action, "allow");
    started.elapsed()
}

fn assert_prompt_record(
    db: &Path,
    outcome: &str,
    action: &str,
    answered_by: &str,
    rule_name: Option<&str>,
) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT prompt_outcome, action, answered_by, rule_name, src_ip, src_port, \
             decided_ts_ms >= ts_ms, repeat_count, rules_seen.id IS NOT NULL \
             FROM events LEFT JOIN rules_seen USING (rule_hash) \
             WHERE process_path = '/usr/bin/prompted'",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let row = rows.next().unwrap().expect("the prompt must be recorded");
    assert_eq!(row.get_ref_unwrap(0).as_str().unwrap(), outcome);
    assert_eq!(row.get_ref_unwrap(1).as_str().unwrap(), action);
    assert_eq!(row.get_ref_unwrap(2).as_str().unwrap(), answered_by);
    assert_eq!(
        row.get::<_, Option<String>>(3).unwrap().as_deref(),
        rule_name
    );
    assert_eq!(row.get_ref_unwrap(4).as_str().unwrap(), "192.0.2.1");
    assert_eq!(row.get::<_, u16>(5).unwrap(), 49321);
    assert!(row.get::<_, bool>(6).unwrap());
    assert_eq!(row.get::<_, u32>(7).unwrap(), 1);
    assert_eq!(row.get::<_, bool>(8).unwrap(), rule_name.is_some());
    assert!(
        rows.next().unwrap().is_none(),
        "each prompt is settled once"
    );
    let snapshots: i64 = conn
        .query_row("SELECT count(*) FROM rules_seen", [], |row| row.get(0))
        .unwrap();
    assert_eq!(snapshots, i64::from(rule_name.is_some()));
}

/// Holds the writer's write lock on `history.db` until dropped, so its next
/// transaction waits (and, past its busy wait, fails).
struct WriterBlock(rusqlite::Connection);

impl WriterBlock {
    fn on(db: &Path) -> Self {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        Self(conn)
    }
}

impl Drop for WriterBlock {
    fn drop(&mut self) {
        let _ = self.0.execute_batch("ROLLBACK");
    }
}

#[tokio::test]
async fn an_enabled_bridge_records_the_decided_events_of_a_ping() {
    let mut boot = boot_enabled().await;
    ping(
        &mut boot.daemon,
        vec![
            event("/usr/bin/curl", "example.com", "allow-curl"),
            event("/usr/bin/wget", "example.org", "allow-wget"),
        ],
    )
    .await;
    assert_eq!(rows_reach(&boot.db, 2).await, 2);
    let conn = rusqlite::Connection::open(&boot.db).unwrap();
    let (name, hash_len): (String, i64) = conn
        .query_row(
            "SELECT rule_name, length(rule_hash) FROM events WHERE process_path = '/usr/bin/curl'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), hash_len), ("allow-curl", 16));
    let bytes = std::fs::read(&boot.db).unwrap();
    assert!(
        !bytes.windows(10).any(|w| w == b"ARGSCANARY"),
        "process_args reached the store"
    );
    boot.bridge.shutdown();
}

#[tokio::test]
async fn each_gui_verdict_records_the_actual_reply_rule_once() {
    for verdict in ["allow", "deny"] {
        let mut boot = boot_enabled().await;
        let ask = spawn_ask(&boot);
        let row_id = pending_row_id(&mut boot.ws).await;
        send_verdict(&mut boot.ws, &row_id, verdict).await;
        // Once the cache publishes the resolved row, loss of the last GUI
        // cannot turn the verdict into an unanswered prompt.
        let updated = frame_with(&mut boot.ws, "updateConnectionRows").await;
        assert_eq!(updated["rows"][0]["id"], row_id);
        assert_eq!(updated["rows"][0]["action"], verdict);
        boot.ws.close(None).await.unwrap();
        let reply = await_ask(ask).await.unwrap();
        assert_eq!(reply.action, verdict);
        assert_eq!(rows_reach(&boot.db, 1).await, 1);
        boot.bridge.shutdown();
        assert_prompt_record(&boot.db, "replied", verdict, "gui", Some(&reply.name));
    }
}

#[tokio::test]
async fn the_last_gui_disconnecting_records_one_gui_lost_outcome() {
    let mut boot = boot_enabled().await;
    let ask = spawn_ask(&boot);
    pending_row_id(&mut boot.ws).await;
    boot.ws.close(None).await.unwrap();
    let error = await_ask(ask).await.unwrap_err();
    let MockError::Rpc(status) = error.as_ref() else {
        panic!("losing the last GUI must end the pending AskRule");
    };
    assert_eq!(
        status.message(),
        "last authenticated GUI session disconnected"
    );
    assert_eq!(rows_reach(&boot.db, 1).await, 1);
    boot.bridge.shutdown();
    assert_prompt_record(&boot.db, "gui-lost", "pending-unanswered", "unknown", None);
}

#[tokio::test]
async fn a_cancelled_daemon_call_records_one_cancelled_outcome() {
    let mut boot = boot_enabled().await;
    let ask = spawn_ask(&boot);
    let row_id = pending_row_id(&mut boot.ws).await;
    ask.abort();
    assert!(ask.await.unwrap_err().is_cancelled());
    let removed = frame_with(&mut boot.ws, "removeConnectionRows").await;
    assert_eq!(removed["ids"], json!([row_id]));
    // Wait for the history writer to drain after the RPC future is dropped.
    assert_eq!(rows_reach(&boot.db, 1).await, 1);
    boot.bridge.shutdown();
    assert_prompt_record(&boot.db, "cancelled", "pending-unanswered", "unknown", None);
}

#[tokio::test]
async fn a_blocked_writer_does_not_delay_a_verdict_or_a_ping() {
    let mut boot = boot_enabled().await;
    // The first batch reaches the writer before the lock, so the writer is
    // alive and idle; then the lock goes on and the next batch waits in it.
    ping(
        &mut boot.daemon,
        vec![event("/usr/bin/a", "a.example", "r")],
    )
    .await;
    assert_eq!(rows_reach(&boot.db, 1).await, 1);
    let block = WriterBlock::on(&boot.db);
    ping(
        &mut boot.daemon,
        vec![event("/usr/bin/b", "b.example", "r")],
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ping_took = ping(
        &mut boot.daemon,
        (0..40)
            .map(|i| event(&format!("/usr/bin/p{i}"), "h.example", "r"))
            .collect(),
    )
    .await;
    assert!(ping_took < VERDICT_BOUND, "a ping took {ping_took:?}");
    let verdict = verdict_latency(&mut boot).await;
    assert!(verdict < VERDICT_BOUND, "a verdict took {verdict:?}");

    drop(block);
    // The writer is still alive afterwards.
    ping(
        &mut boot.daemon,
        vec![event("/usr/bin/z", "z.example", "r")],
    )
    .await;
    assert!(rows_reach(&boot.db, 2).await >= 2);
    boot.bridge.shutdown();
}

#[tokio::test]
async fn a_full_queue_does_not_delay_a_verdict_or_a_ping() {
    let mut boot = boot_enabled().await;
    ping(
        &mut boot.daemon,
        vec![event("/usr/bin/a", "a.example", "r")],
    )
    .await;
    assert_eq!(rows_reach(&boot.db, 1).await, 1);
    let block = WriterBlock::on(&boot.db);
    // Worst-case records (a 4,000 byte path each) from distinct processes,
    // so admission lets them through and only the 2 MiB queue bound stops
    // them: about 380 fit, with the writer waiting on its lock.
    let sent = 600;
    let big = |i: usize| format!("/opt/{i}/{}", "p".repeat(3900));
    let flood = ping(
        &mut boot.daemon,
        (0..sent)
            .map(|i| with_operand(&big(i), "h.example", "r", "/opt"))
            .collect(),
    )
    .await;
    assert!(flood < VERDICT_BOUND, "the flooding ping took {flood:?}");
    let verdict = verdict_latency(&mut boot).await;
    assert!(verdict < VERDICT_BOUND, "a verdict took {verdict:?}");

    drop(block);
    ping(
        &mut boot.daemon,
        vec![event("/usr/bin/z", "z.example", "r")],
    )
    .await;
    let stored = rows_reach(&boot.db, 2).await;
    assert!(stored >= 2, "the writer recovered: {stored}");
    assert!(
        stored < i64::from(sent as i32),
        "some of the flood was dropped or lost, not all stored: {stored}"
    );
    boot.bridge.shutdown();
}
