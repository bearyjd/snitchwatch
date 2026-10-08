//! End to end, prompt-slot plan Part C, with the mock daemon and a
//! WebSocket GUI (same boot pattern as `prompt_slot_test.rs`):
//! - a held `AskRule` nobody answers gets `Unavailable` once the answer
//!   timeout passes (shortened here; the real one is 30 s), so the daemon
//!   applies its default action and stores no rule. The GUI saw a countdown
//!   on the pending row, then the row deferred and labelled;
//! - "Decide later" on a program the bridge can name answers at once with a
//!   5 minute deny for that program on any host.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::{MockError, MockOpensnitchd};
use serde_json::{json, Value};
use snitchwatch_bridge_cli::{run_with_answer_timeout, BridgeConfig, RunningBridge};
use snitchwatch_proto::protocol::{ClientConfig, Connection};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Short, for the test that waits for it.
const ANSWER_TIMEOUT: Duration = Duration::from_millis(400);
/// Long, for the test that must be answered before it.
const LONG_ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Frames up to and including the next one with `action`.
async fn frames_until(ws: &mut WebSocketStream<UnixStream>, action: &str) -> Vec<Value> {
    let mut frames = Vec::new();
    loop {
        let frame = next_frame(ws).await;
        let done = frame["action"] == action;
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

/// A bridge, a GUI that has authenticated, and a daemon that subscribed
/// with `DefaultAction: deny`.
async fn boot(
    answer_timeout: Duration,
) -> (
    RunningBridge,
    WebSocketStream<UnixStream>,
    tempfile::TempDir,
    std::net::SocketAddr,
) {
    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run_with_answer_timeout(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: socket_dir.path().join("bridge.sock"),
            cache_capacity: 1024,
        },
        answer_timeout,
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
    let ack = next_frame(&mut ws).await;
    assert_eq!(ack["action"], "authenticated");
    assert!(
        ack["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("decideLater")),
        "{ack}"
    );
    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    MockOpensnitchd::connect(grpc_addr)
        .await
        .unwrap()
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            config: r#"{"DefaultAction": "deny"}"#.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    (bridge, ws, socket_dir, grpc_addr)
}

fn ask(
    grpc_addr: std::net::SocketAddr,
    process_path: &str,
) -> tokio::task::JoinHandle<Result<snitchwatch_proto::protocol::Rule, MockError>> {
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: "updates.example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: process_path.into(),
        ..Default::default()
    };
    tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        daemon.ask_rule(conn).await
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn an_unanswered_ask_gets_no_answer_and_its_row_says_so() {
    let (bridge, mut ws, _dir, grpc_addr) = boot(ANSWER_TIMEOUT).await;
    let before = now_ms();
    let held = ask(grpc_addr, "/usr/bin/background-agent");
    let inserted = frames_until(&mut ws, "insertConnectionRows").await;
    let row = &inserted.last().unwrap()["rows"][0];
    assert_eq!(row["action"], Value::Null, "the Ask is waiting");
    let deadline = row["answerDeadlineMs"].as_i64().expect("a countdown");
    assert!(
        deadline >= before + ANSWER_TIMEOUT.as_millis() as i64,
        "{row}"
    );
    let row_id = row["id"].as_str().unwrap().to_string();

    let error = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("the Ask was not answered at the timeout")
        .unwrap()
        .expect_err("no answer is an error reply");
    match error {
        MockError::Rpc(status) => {
            assert_eq!(status.code(), tonic::Code::Unavailable);
            assert_eq!(status.message(), "no answer");
        }
        other => panic!("expected an Unavailable status, got {other}"),
    }

    let mut frames = frames_until(&mut ws, "updateConnectionRows").await;
    let updated = &frames.last().unwrap()["rows"][0];
    assert_eq!(updated["id"], row_id.as_str());
    assert_eq!(updated["deferred"], true);
    assert_eq!(updated["autoAnswer"], "noAnswer");
    assert_eq!(updated["action"], "deny", "the daemon's DefaultAction");
    assert_eq!(updated["answerDeadlineMs"], Value::Null);
    loop {
        let frame = next_frame(&mut ws).await;
        let released = frame["action"] == "promptSlot" && frame["holders"] == 0;
        frames.push(frame);
        if released {
            break;
        }
    }
    assert!(
        !frames.iter().any(|f| f["action"] == "updateRules"),
        "no rule is saved: {frames:?}"
    );
    bridge.shutdown();
}

#[tokio::test]
async fn decide_later_blocks_the_program_for_five_minutes_at_once() {
    let (bridge, mut ws, _dir, grpc_addr) = boot(LONG_ANSWER_TIMEOUT).await;
    let held = ask(grpc_addr, "/usr/bin/background-agent");
    let inserted = frames_until(&mut ws, "insertConnectionRows").await;
    let row_id = inserted.last().unwrap()["rows"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    ws.send(Message::Text(
        json!({ "action": "decideLater", "rowId": row_id }).to_string(),
    ))
    .await
    .unwrap();
    // Well inside the answer timeout.
    let rule = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("Decide later did not answer at once")
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "deny");
    assert_eq!(rule.duration, "5m");
    let operator = rule.operator.unwrap();
    assert_eq!(operator.operand, "process.path");
    assert_eq!(operator.data, "/usr/bin/background-agent");

    let frames = frames_until(&mut ws, "updateConnectionRows").await;
    let updated = &frames.last().unwrap()["rows"][0];
    assert_eq!(updated["id"], row_id.as_str());
    assert_eq!(updated["deferred"], true);
    assert_eq!(updated["action"], "deny");
    assert_eq!(updated["autoAnswer"], Value::Null, "a person chose this");
    bridge.shutdown();
}
