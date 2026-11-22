//! PR 5, through a real bridge and the mock daemon: an enabled history
//! records a ping's decided events, and a blocked writer or a full queue
//! never delays a prompt's verdict or a ping's reply.
//!
//! Its own test binary: `history_off.rs` asserts process-wide facts (no writer
//! thread, no queue) that an enabled bridge here would break.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_bridge::history::{enable, locate, Consent, Mode};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{ClientConfig, Connection, Event, Operator, Rule, Statistics};
use tokio::net::UnixStream;
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
        loop {
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                return serde_json::from_str(&text).unwrap();
            }
        }
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

/// An AskRule from the mock daemon, answered by the GUI; how long the
/// answer took from the GUI's `setVerdict`.
async fn verdict_latency(boot: &mut Boot) -> Duration {
    let grpc_path = boot.grpc_path.clone();
    let ask = tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect_unix(&grpc_path).await.unwrap();
        daemon
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "updates.example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/prompted".into(),
                ..Default::default()
            })
            .await
    });
    // The pings' decided rows were broadcast too; the prompt's row has no
    // action yet.
    let row_id = loop {
        let inserted = frame_with(&mut boot.ws, "insertConnectionRows").await;
        let row = &inserted["rows"][0];
        if row["action"] == Value::Null {
            break row["id"].as_str().unwrap().to_string();
        }
    };
    let started = Instant::now();
    boot.ws
        .send(Message::Text(
            json!({
                "action": "setVerdict",
                "rowId": row_id,
                "verdict": "allow",
                "scope": "this_host",
                "duration": "always",
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let rule = tokio::time::timeout(Duration::from_secs(10), ask)
        .await
        .expect("the prompt was never answered")
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "allow");
    started.elapsed()
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
