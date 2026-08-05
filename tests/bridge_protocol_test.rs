//! End-to-end test: mock_opensnitchd (gRPC client) ↔ bridge (gRPC server +
//! WS server) ↔ WebSocket UI client.
//!
//! This is the M1.5 acceptance test. It proves that:
//!   1. The bridge binds both its gRPC and WebSocket ports.
//!   2. opensnitchd (mocked) can dial in and call `AskRule`.
//!   3. The bridge broadcasts an `InsertConnectionRows` to any connected WS
//!      client carrying the new pending row.
//!   4. A WS `setVerdict` resolves the pending row, and the original
//!      `AskRule` unary call returns a `Rule` whose `action` matches.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::json;
use snitchwatch_bridge::daemon_watchdog::DAEMON_DOWN_TIMEOUT;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::Connection;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Connect to the bridge's `/stream` route over its Unix domain socket and
/// present the handshake token as the first frame, exactly as a real client
/// (or `crate::loopback_proxy` on the Tauri shell's behalf) must.
async fn connect_stream(socket_path: &std::path::Path, token: &str) -> WebSocketStream<UnixStream> {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("unix socket connect failed");
    let (mut ws, _resp) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .expect("ws handshake failed");
    ws.send(Message::Text(token.to_string()))
        .await
        .expect("token handshake send failed");
    ws
}

#[tokio::test]
async fn ask_rule_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Boot the bridge: ephemeral gRPC port + a Unix socket under a fresh
    //    temp dir for the WS server.
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // 2. Connect a WebSocket client (presenting the handshake token) BEFORE
    //    the AskRule call so we don't miss the broadcast.
    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    // 3. Spawn an opensnitchd mock client and fire AskRule in the background.
    let grpc_addr = bridge.grpc_addr;
    let ask_handle = tokio::spawn(async move {
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

    // 4. Wait for the InsertConnectionRows broadcast on the WS.
    let insert_msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value =
                        serde_json::from_str(&t).expect("server sent bad json");
                    if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                        break v;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for insertConnectionRows");

    let rows = insert_msg
        .get("rows")
        .and_then(|r| r.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let row_id = row
        .get("id")
        .and_then(|v| v.as_str())
        .expect("row id")
        .to_string();
    assert_eq!(row.get("process").and_then(|v| v.as_str()), Some("curl"));
    assert_eq!(
        row.get("dstHost").and_then(|v| v.as_str()),
        Some("example.com")
    );
    assert_eq!(row.get("dstPort").and_then(|v| v.as_u64()), Some(443));
    assert!(
        row.get("action").map(|v| v.is_null()).unwrap_or(false),
        "pending rows must have action: null"
    );

    // 5. Send a SetVerdict to decide the pending row.
    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "allow",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string()))
        .await
        .expect("ws send failed");

    // 6. The mock's AskRule call should now return with action=allow.
    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .expect("ask_rule timed out")
        .expect("ask_rule task panicked");
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(!rule.name.is_empty(), "daemon rejects empty rule names");

    bridge.shutdown();
}

#[tokio::test]
async fn deny_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_addr;
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "udp".into(),
            dst_host: "tracker.bad".into(),
            dst_ip: "1.2.3.4".into(),
            dst_port: 53,
            process_path: "/usr/bin/dnsmasq".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    // Drain WS until we see the insert.
    let row_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                    return v["rows"][0]["id"].as_str().unwrap().to_string();
                }
            }
        }
    })
    .await
    .unwrap();

    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "deny",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string())).await.unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "deny");

    bridge.shutdown();
}

#[tokio::test]
async fn diagnostics_report_reflects_firewall_down_after_subscribe() {
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_addr;
    let subscribe_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.subscribe_with_config(snitchwatch_proto::protocol::ClientConfig {
            id: 1,
            name: "mock".to_string(),
            version: "mock-1.6.0".to_string(),
            is_firewall_running: false,
            ..Default::default()
        })
        .await
        .unwrap();
    });
    subscribe_handle.await.unwrap();

    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .expect("send requestSnapshot failed");

    let mut saw_firewall_failed = false;
    for _ in 0..20 {
        let Some(Ok(Message::Text(text))) = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("timed out waiting for a WS message")
        else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        if v.get("action").and_then(|a| a.as_str()) == Some("diagnosticsReport") {
            let checks = v["checks"].as_array().unwrap();
            saw_firewall_failed = checks
                .iter()
                .any(|c| c["kind"] == "firewall_running" && c["status"]["status"] == "failed");
            break;
        }
    }
    assert!(
        saw_firewall_failed,
        "expected a diagnosticsReport with a failed firewall_running check"
    );

    bridge.shutdown();
}

/// The load-bearing test for issue #5: a real opensnitchd only pings when it
/// has new stats events (see `daemon_watchdog`'s module doc), so an idle
/// daemon stays connected but silent. Holding the `Notifications` stream
/// open with zero pings for well past `DAEMON_DOWN_TIMEOUT` must NOT
/// false-positive a `DaemonDown` transition, and the diagnostics report must
/// still claim the daemon reachable.
///
/// Uses real wall-clock sleeps (not a paused tokio clock): this test spins
/// up real TCP/Unix-socket IO end to end, and tokio's paused-clock
/// auto-advance can fire a connect timeout before real IO completes when
/// mixed with genuine sockets — see the sibling test's identical choice.
#[tokio::test]
async fn idle_daemon_with_open_notifications_stream_stays_reachable() {
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let mut bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    // Mock opensnitchd connects and opens the Notifications stream, but
    // never calls ping() — exactly the idle-but-connected shape observed
    // live from a real daemon.
    let mut mock = MockOpensnitchd::connect(bridge.grpc_addr).await.unwrap();
    let (_reply_tx, _count_rx) = mock.open_notifications().await.unwrap();

    // Mark the current value seen (Idle, from bridge startup) so a later
    // `has_changed()` reflects only transitions from here on — asserting
    // the *final* value with `borrow()` alone can't catch a transient
    // DaemonDown->Idle flap that self-corrects before we check.
    bridge.tray_rx.borrow_and_update();

    // Sleep well past the timeout, across several watchdog ticks, with
    // zero pings.
    tokio::time::sleep(DAEMON_DOWN_TIMEOUT + Duration::from_secs(3)).await;

    // The tray must never have changed at all while the idle daemon held
    // the Notifications stream open — not just "ended up Idle again".
    assert!(
        !bridge.tray_rx.has_changed().unwrap(),
        "tray state changed while idle daemon held stream open"
    );

    // A fresh diagnostics report must still claim the daemon reachable.
    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .expect("send requestSnapshot failed");

    let mut saw_daemon_reachable_ok = false;
    for _ in 0..20 {
        let Some(Ok(Message::Text(text))) = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("timed out waiting for a WS message")
        else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        if v.get("action").and_then(|a| a.as_str()) == Some("diagnosticsReport") {
            let checks = v["checks"].as_array().unwrap();
            saw_daemon_reachable_ok = checks
                .iter()
                .any(|c| c["kind"] == "daemon_reachable" && c["status"]["status"] == "ok");
            break;
        }
    }
    assert!(
        saw_daemon_reachable_ok,
        "expected a diagnosticsReport with daemon_reachable: ok while the \
         Notifications stream stays open with zero pings"
    );

    bridge.shutdown();
}

/// Second half of issue #5's fix: once the daemon's side of the
/// Notifications stream closes, the down transition must fire promptly —
/// within one watchdog tick — rather than waiting out a fresh
/// `DAEMON_DOWN_TIMEOUT` countdown from the close. `last_activity` was
/// already stale (kept "alive" only by the open-stream override); closing
/// the stream just removes that override, so the existing staleness applies
/// immediately.
///
/// Real wall-clock sleeps, not a paused clock — see the previous test's doc
/// comment for why.
#[tokio::test]
async fn notifications_stream_close_triggers_down_transition_within_one_tick() {
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let mut bridge = run(cfg).await.expect("bridge run failed");

    let mut mock = MockOpensnitchd::connect(bridge.grpc_addr).await.unwrap();
    let (reply_tx, _count_rx) = mock.open_notifications().await.unwrap();

    // Let last_activity age well past DAEMON_DOWN_TIMEOUT while the stream
    // stays open, proving (as in the previous test) that staleness alone
    // doesn't trip the watchdog here.
    tokio::time::sleep(DAEMON_DOWN_TIMEOUT + Duration::from_secs(3)).await;
    assert_eq!(*bridge.tray_rx.borrow_and_update(), TrayState::Idle);

    let mut broadcast_rx = bridge.broadcast_tx.subscribe();

    // Close the daemon's side of the stream (mirrors the daemon process
    // exiting / dropping the RPC).
    drop(reply_tx);

    // A single watchdog tick should be enough — no further ping-staleness
    // wait is needed since last_activity is already stale. Bounded by an
    // explicit timeout so a regression here fails the test instead of
    // hanging CI.
    tokio::time::timeout(Duration::from_secs(10), bridge.tray_rx.changed())
        .await
        .expect("tray never transitioned after Notifications stream closed")
        .unwrap();
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::DaemonDown);

    let msg = tokio::time::timeout(Duration::from_secs(3), broadcast_rx.recv())
        .await
        .expect("timed out waiting for DiagnosticsReport")
        .unwrap();
    assert!(matches!(msg, ServerMessage::DiagnosticsReport { .. }));

    bridge.shutdown();
}

/// The load-bearing test for issue #6: a real opensnitchd's own `PostAlert`
/// RPC is the only signal that surfaces a daemon-internal failure (eBPF
/// module load, in this case) a host-side kernel probe can't see — verified
/// live 2026-07-31 where the probe reported BTF present while opensnitchd
/// still failed to load its bundled eBPF module.
///
/// Drives the alert as `Alert_GENERIC` + free text, not a tagged
/// `PROC_MONITOR` alert: that's the only shape a real v1.8.0 daemon actually
/// sends (`SendWarningAlert`/`SendErrorAlert` hardcode `Alert_GENERIC` —
/// see `daemon_alerts`'s module doc), and the diagnostics overlay must
/// text-classify it to `ebpf_support` on its own.
///
/// Proves: (a) the alert pushes an *unsolicited* `diagnosticsReport` with
/// `ebpf_support: failed` carrying the daemon's own alert text, without the
/// GUI having to poll/recheck; (b) a fresh `subscribe()` (a new daemon
/// session, e.g. a plain reconnect) does NOT clear the stored alert — it's
/// still `failed` afterwards; (c) only an explicit `recheckDiagnostics`
/// (the user-driven "re-baseline") clears it, after which the detail no
/// longer carries the daemon's alert text (this test intentionally does
/// NOT assert an exact `ok` status for `ebpf_support` post-clear — that
/// depends on this host's actual BTF support, which the local probe reports
/// independently of anything this test controls).
#[tokio::test]
async fn generic_alert_fails_ebpf_check_persists_across_subscribe_clears_on_recheck() {
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let mut mock = MockOpensnitchd::connect(bridge.grpc_addr).await.unwrap();

    // vendor/opensnitch/daemon/main.go:645 — the real string opensnitchd
    // v1.8.0 sends on this exact failure.
    const ALERT_TEXT: &str =
        "Unable to set process monitor method via parameter: exec format error";
    mock.post_alert_text(
        snitchwatch_proto::protocol::alert::Type::Error,
        snitchwatch_proto::protocol::alert::What::Generic,
        ALERT_TEXT,
    )
    .await
    .expect("post_alert failed");

    // (a) Unsolicited: no requestSnapshot/recheck sent — post_alert itself
    // must push the fresh report.
    let checks = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v.get("action").and_then(|a| a.as_str()) == Some("diagnosticsReport") {
                        return v["checks"].as_array().unwrap().clone();
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for the unsolicited diagnosticsReport");

    let ebpf = checks
        .iter()
        .find(|c| c["kind"] == "ebpf_support")
        .expect("no ebpf_support check in report");
    assert_eq!(ebpf["status"]["status"], "failed");
    assert!(
        ebpf["status"]["detail"]
            .as_str()
            .unwrap()
            .contains(ALERT_TEXT),
        "expected ebpf_support detail to carry the daemon's alert text, got: {ebpf}"
    );

    // (b) A fresh subscribe() (a plain reconnect) must NOT clear the alert.
    mock.subscribe("opensnitchd-resubscribe").await.unwrap();

    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .expect("send requestSnapshot failed");

    let still_failed = wait_for_ebpf_status(&mut ws).await;
    assert_eq!(
        still_failed["status"]["status"], "failed",
        "subscribe() must not have cleared the stored alert"
    );
    assert!(
        still_failed["status"]["detail"]
            .as_str()
            .unwrap()
            .contains(ALERT_TEXT),
        "alert text must still be present after a plain subscribe()"
    );

    // (c) An explicit recheckDiagnostics DOES clear it.
    ws.send(Message::Text(
        json!({ "action": "recheckDiagnostics" }).to_string(),
    ))
    .await
    .expect("send recheckDiagnostics failed");

    let cleared = wait_for_ebpf_status(&mut ws).await;
    let detail = cleared["status"]["detail"].as_str().unwrap_or("");
    assert!(
        !detail.contains(ALERT_TEXT) && !detail.contains("opensnitchd reports"),
        "expected the daemon-alert overlay gone after recheckDiagnostics, got: {cleared}"
    );

    bridge.shutdown();
}

/// Waits for the next `diagnosticsReport` WS message and returns its
/// `ebpf_support` check entry.
async fn wait_for_ebpf_status(ws: &mut WebSocketStream<UnixStream>) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v.get("action").and_then(|a| a.as_str()) == Some("diagnosticsReport") {
                        let checks = v["checks"].as_array().unwrap();
                        return checks
                            .iter()
                            .find(|c| c["kind"] == "ebpf_support")
                            .expect("no ebpf_support check in report")
                            .clone();
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for a diagnosticsReport")
}

/// Rule enable/disable/delete must actually reach the daemon.
///
/// Before this path existed the bridge produced `UpstreamEffect::UpdateRule`
/// and `DeleteRule` and then dropped them: the outbound `Notifications` stream
/// was parked on `std::future::pending()`, so nothing was ever sent. The Rules
/// page's controls were wired end to end right up to the bridge and stopped
/// there.
///
/// Asserts the daemon-visible result, not just "a notification arrived":
///   * the action is `CHANGE_RULE` / `DELETE_RULE`,
///   * the toggled `enabled` value survives (the entire point of the command),
///   * the rule passes `validate_rule_shape`, which mirrors the daemon's real
///     acceptance path — `Deserialize` *and* `Operator.Compile()`. A rule that
///     fails it is silently discarded by a real daemon, which is what issue #14
///     was, and would make this feature look like it works while doing nothing.
///   * the type is never `NONE`, which would order the daemon to close the
///     stream (`vendor/opensnitch/daemon/ui/notifications.go:405-408`).
#[tokio::test]
async fn rule_update_and_delete_reach_the_daemon_as_notifications() {
    use snitchwatch_bridge::ws_messages::ClientMessage;
    use snitchwatch_proto::protocol::Action;

    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // The stream must be open *before* the effect is sent: the bridge
    // broadcasts to whoever is subscribed and replays nothing.
    let mut mock = MockOpensnitchd::connect(bridge.grpc_addr).await.unwrap();
    let (_reply_tx, mut notifications) = mock.open_notifications().await.unwrap();

    // Shaped exactly like `RulesStore::toggled_rule_json` output: the full
    // rule with `enabled` already flipped to the desired value.
    let toggled = json!({
        "name": "899-firefox-allow-out",
        "enabled": false,
        "action": "allow",
        "duration": "always",
        "description": "",
        "operator": {
            "type": "simple",
            "operand": "dest.host",
            "data": "example.com",
            "sensitive": false,
        },
    });

    bridge
        .inbound_tx
        .send(ClientMessage::UpdateRule {
            rule_id: "899-firefox-allow-out".to_string(),
            rule: toggled,
        })
        .await
        .expect("inbound channel closed");

    let change = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("no CHANGE_RULE notification reached the daemon")
        .expect("notification stream closed");

    assert_eq!(
        change.r#type,
        Action::ChangeRule as i32,
        "rule toggle must arrive as CHANGE_RULE"
    );
    assert_ne!(
        change.r#type,
        Action::None as i32,
        "a NONE-typed notification would close the daemon's stream"
    );
    assert_ne!(change.id, 0, "id 0 collides with the daemon's HELLO reply");
    assert_eq!(change.rules.len(), 1);
    assert_eq!(change.rules[0].name, "899-firefox-allow-out");
    assert!(
        !change.rules[0].enabled,
        "the toggled `enabled` value must survive to the daemon"
    );
    mock_opensnitchd::validate_rule_shape(&change.rules[0])
        .expect("a real daemon would silently reject this rule");

    // Delete needs only the name (notifications.go:132).
    bridge
        .inbound_tx
        .send(ClientMessage::DeleteRule {
            rule_id: "899-firefox-allow-out".to_string(),
        })
        .await
        .expect("inbound channel closed");

    let delete = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("no DELETE_RULE notification reached the daemon")
        .expect("notification stream closed");

    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules.len(), 1);
    assert_eq!(delete.rules[0].name, "899-firefox-allow-out");
    assert!(
        delete.id > change.id,
        "notification ids must increase so daemon replies can be correlated"
    );

    bridge.shutdown();
}
