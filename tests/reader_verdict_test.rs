//! History PR 9, test 22: reads never block a verdict. Two history readers
//! read flat out on their own thread pool and file while an `AskRule` goes
//! through the mock daemon and a WebSocket GUI; the answer comes at once. (The
//! reader is not wired into the bridge yet: PR 11 re-runs this over the real
//! WebSocket.)

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::{json, Value};
use snitchwatch_bridge::history::lifecycle::Generation;
use snitchwatch_bridge::history::reader::{read_page, Context, Limits, Outcome, Request};
use snitchwatch_bridge::history::reader_registry::CancelToken;
use snitchwatch_bridge::history::{enable, locate, open_store, Consent, Mode};
use snitchwatch_bridge_cli::{run_with_answer_timeout, BridgeConfig};
use snitchwatch_proto::protocol::{ClientConfig, Connection};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

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

/// A history store with 20,000 events, enabled the supported way.
fn history_db(dir: &std::path::Path) -> std::path::PathBuf {
    let state = dir.canonicalize().unwrap().join("state");
    std::fs::create_dir(&state).unwrap();
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let store = open_store(&location).unwrap();
    store
        .connection()
        .execute_batch(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 20000) \
             INSERT INTO events (ts_ms, action, answered_by, process_path) \
             SELECT x, 'allow', 'rule', '/usr/bin/old' FROM c;",
        )
        .unwrap();
    location.db_path()
}

#[tokio::test]
async fn an_ask_is_answered_at_once_while_two_readers_read_flat_out() {
    let history_dir = tempfile::tempdir().unwrap();
    let db = history_db(history_dir.path());
    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run_with_answer_timeout(
        BridgeConfig {
            cache_capacity: 1024,
            ..BridgeConfig::user_unix_for_tests(socket_dir.path())
        },
        Duration::from_secs(60),
    )
    .await
    .expect("bridge run failed");
    let stream = UnixStream::connect(&bridge.ws_socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(bridge.ws_token.as_str().to_string()))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut ws).await["action"], "authenticated");
    let grpc_path = bridge.grpc_endpoint.unix_path().unwrap().to_path_buf();
    MockOpensnitchd::connect_unix(&grpc_path)
        .await
        .unwrap()
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            config: r#"{"DefaultAction": "deny"}"#.into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let done = std::sync::Arc::new(AtomicBool::new(false));
    let reads = std::sync::Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (db, done, reads) = (db.clone(), done.clone(), reads.clone());
            std::thread::spawn(move || {
                let (generation, cancel) = (Generation::default(), CancelToken::default());
                let ctx = Context {
                    db_path: &db,
                    generation: &generation,
                    cancel: &cancel,
                    now_ms: 1_000_000,
                    live_session_start_ms: None,
                    standing: None,
                    limits: Limits::default(),
                };
                while !done.load(Ordering::SeqCst) {
                    let outcome = read_page(
                        &Request {
                            cursor: None,
                            limit: 200,
                        },
                        &ctx,
                    );
                    assert!(matches!(outcome, Outcome::Page(_)), "{outcome:?}");
                    reads.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    // Readers are demonstrably mid-flight before the Ask goes in.
    while reads.load(Ordering::SeqCst) < 4 {
        tokio::task::yield_now().await;
    }

    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: "updates.example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: "/usr/bin/background-agent".into(),
        ..Default::default()
    };
    let held = tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect_unix(&grpc_path).await.unwrap();
        daemon.ask_rule(conn).await
    });
    let row_id = loop {
        let frame = next_frame(&mut ws).await;
        if frame["action"] == "insertConnectionRows" {
            break frame["rows"][0]["id"].as_str().unwrap().to_string();
        }
    };
    let started = Instant::now();
    ws.send(Message::Text(
        json!({ "action": "decideLater", "rowId": row_id }).to_string(),
    ))
    .await
    .unwrap();
    let rule = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("the Ask was not answered while readers ran")
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "deny");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    done.store(true, Ordering::SeqCst);
    for reader in readers {
        reader.join().unwrap();
    }
    bridge.shutdown();
}
