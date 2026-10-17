use super::*;
use snitchwatch_bridge::ws_messages::{
    BlocklistEntry, BlocklistSummary, ConnectionRow, TrafficEvent, VerdictAction, VerdictDuration,
    VerdictScope,
};

fn conn_row(id: &str) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: "firefox".into(),
        process_path: None,
        dst_host: "github.com".into(),
        dst_ip: "1.1.1.1".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "outgoing".into(),
        action: None,
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

#[test]
fn connection_messages_route_only_to_connections() {
    let msg = ServerMessage::InsertConnectionRows {
        rows: vec![conn_row("r1")],
    };
    assert!(interests_connections(&msg));
    assert!(!interests_rules(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(!interests_blocklist_entries(&msg));

    assert!(interests_connections(&ServerMessage::ClearConnectionRows));
    assert!(interests_connections(&ServerMessage::MoveConnetionRows {
        ids: vec!["r1".into()]
    }));
}

#[test]
fn connection_messages_also_route_to_geo() {
    let msg = ServerMessage::InsertConnectionRows {
        rows: vec![conn_row("r1")],
    };
    assert!(interests_geo(&msg));
    assert!(interests_geo(&ServerMessage::UpdateConnectionRows {
        rows: vec![conn_row("r1")]
    }));
    assert!(interests_geo(&ServerMessage::RemoveConnectionRows {
        ids: vec!["r1".into()]
    }));
    assert!(interests_geo(&ServerMessage::MoveConnetionRows {
        ids: vec!["r1".into()]
    }));
    assert!(interests_geo(&ServerMessage::ClearConnectionRows));
}

#[test]
fn unrelated_messages_do_not_route_to_geo() {
    assert!(!interests_geo(&ServerMessage::SetRules { rules: vec![] }));
    assert!(!interests_geo(&ServerMessage::SetConnectionsStatus {
        status: snitchwatch_bridge::ws_messages::ConnectionsStatus::Connected
    }));
}

#[test]
fn rule_messages_route_only_to_rules() {
    let msg = ServerMessage::SetRules { rules: vec![] };
    assert!(interests_rules(&msg));
    assert!(!interests_connections(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(interests_rules(&ServerMessage::UpdateRules {
        rules: vec![]
    }));
    let not_shown = ServerMessage::RulesNotShown {
        too_large: 1,
        over_limit_total: None,
        listed: true,
        left_on_disk: 0,
        count_mismatch: false,
    };
    assert!(interests_rules(&not_shown));
    assert!(!interests_connections(&not_shown));
}

#[test]
fn rule_hit_counts_route_only_to_rules() {
    let msg = ServerMessage::RuleHits {
        since_unix_ms: None,
        lossy: false,
        last_gap_unix_ms: None,
        storage: snitchwatch_bridge::ws_messages::StorageStatus {
            persistent: false,
            reason: None,
            unreadable: false,
        },
        hits: vec![],
    };
    assert!(interests_rules(&msg));
    assert!(!interests_connections(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(!interests_blocklist_entries(&msg));
    assert!(!interests_geo(&msg));
}

#[test]
fn blocklist_subscription_messages_route_only_to_blocklists() {
    let msg = ServerMessage::SetBlocklists {
        blocklists: vec![BlocklistSummary {
            id: "sb".into(),
            display_name: "StevenBlack".into(),
            url: "https://x/hosts".into(),
            entry_count: 1,
            status: "ok".into(),
            last_updated_iso8601: None,
            last_failure_reason: None,
            enforcement: String::new(),
            enforcement_reason: None,
        }],
        storage: None,
    };
    assert!(interests_blocklists(&msg));
    assert!(!interests_blocklist_entries(&msg));
    assert!(!interests_connections(&msg));

    assert!(interests_blocklists(&ServerMessage::SetBlocklistStatus {
        subscription_id: "sb".into(),
        status: "fetching".into(),
        last_failure_reason: None,
    }));
    let leftovers: ServerMessage =
        serde_json::from_str(r#"{"action":"setBlocklistLeftovers","count":2}"#).unwrap();
    assert!(interests_blocklists(&leftovers));
    assert!(!interests_rules(&leftovers));
}

#[test]
fn blocklist_entry_messages_route_only_to_entries() {
    let msg = ServerMessage::SetBlocklistEntries {
        subscription_id: "sb".into(),
        entries: vec![BlocklistEntry {
            host: "doubleclick.net".into(),
        }],
        offset: 0,
        total: 1,
        request_id: None,
        last_updated_iso8601: None,
    };
    assert!(interests_blocklist_entries(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(!interests_connections(&msg));
}

#[test]
fn profile_messages_route_only_to_profiles() {
    use snitchwatch_bridge::ws_messages::{ProfileRuleWire, ProfileSummary};

    let msg = ServerMessage::SetProfiles {
        profiles: vec![ProfileSummary {
            id: "home".into(),
            name: "At Home".into(),
            network_matchers: vec!["Home*".into()],
            rules: vec![ProfileRuleWire {
                id: "r1".into(),
                action: "allow".into(),
                operand: "dest.host".into(),
                data: "nas.local".into(),
                ..Default::default()
            }],
            active: true,
        }],
        storage: None,
        applies_rules: false,
        not_applied_reason: None,
    };
    assert!(interests_profiles(&msg));
    assert!(!interests_connections(&msg));
    assert!(!interests_rules(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(!interests_blocklist_entries(&msg));
    assert!(!interests_traffic(&msg));

    assert!(interests_profiles(&ServerMessage::ProfileChanged {
        active_profile_id: Some("home".into())
    }));
}

#[test]
fn traffic_event_messages_route_only_to_traffic() {
    let msg = ServerMessage::TrafficEvents {
        events: vec![TrafficEvent {
            timestamp_ms: 1_000_000_000_000,
            bytes_in: 100,
            bytes_out: 50,
        }],
    };
    assert!(interests_traffic(&msg));
    assert!(!interests_connections(&msg));
    assert!(!interests_rules(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(!interests_blocklist_entries(&msg));
}

#[test]
fn daemon_statistics_messages_route_only_to_traffic() {
    let msg = ServerMessage::DaemonStatistics {
        daemon_version: "1.8.0".into(),
        uptime: 3661,
        rules: 12,
        connections: 4200,
        ignored: 10,
        accepted: 4000,
        dropped: 200,
        rule_hits: 3900,
        rule_misses: 300,
    };
    assert!(interests_traffic(&msg));
    assert!(!interests_connections(&msg));
    assert!(!interests_rules(&msg));
    assert!(!interests_blocklists(&msg));
    assert!(!interests_blocklist_entries(&msg));
    assert!(!interests_profiles(&msg));
    assert!(!interests_diagnostics(&msg));
}

#[test]
fn legacy_uplot_traffic_blobs_are_not_routed_to_traffic() {
    assert!(!interests_traffic(&ServerMessage::SetTrafficData {
        data: serde_json::json!({}),
    }));
    assert!(!interests_traffic(&ServerMessage::UpdateTrafficData {
        data: serde_json::json!({}),
    }));
}

#[test]
fn diagnostics_route_only_to_diagnostics_report() {
    let diagnostics_msg = ServerMessage::DiagnosticsReport { checks: vec![] };
    assert!(interests_diagnostics(&diagnostics_msg));

    let other_msg = ServerMessage::ClearConnectionRows;
    assert!(!interests_diagnostics(&other_msg));
}

#[test]
fn deny_scope_narrowed_routes_only_to_itself() {
    let msg = ServerMessage::DenyScopeNarrowed {
        row_id: "ask-1".into(),
        reason: "the destination is a bare IP address, not a hostname".into(),
    };
    assert!(interests_deny_scope_narrowed(&msg));
    assert!(!interests_diagnostics(&msg));
    assert!(!interests_connections(&msg));

    assert!(!interests_deny_scope_narrowed(
        &ServerMessage::ClearConnectionRows
    ));
}

#[test]
fn encode_server_round_trips_through_apply_json_shape() {
    // The JSON the feed hands to `applyServerMessageJson` must deserialize
    // back to the identical typed message the models parse internally.
    let msg = ServerMessage::InsertConnectionRows {
        rows: vec![conn_row("r7")],
    };
    let json = encode_server(&msg).expect("encode");
    let back: ServerMessage = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, msg);
}

#[test]
fn decode_client_parses_model_emitted_verdict_json() {
    // Exactly the JSON `pending_decision::build_verdict_message` produces.
    let json = r#"{"action":"setVerdict","rowId":"r1","verdict":"deny","scope":"any_host","duration":"always"}"#;
    match decode_client(json).expect("decode") {
        ClientMessage::SetVerdict {
            row_id,
            verdict,
            scope,
            duration,
            remember,
        } => {
            assert_eq!(row_id, "r1");
            assert_eq!(verdict, VerdictAction::Deny);
            assert_eq!(scope, VerdictScope::AnyHost);
            assert_eq!(duration, Some(VerdictDuration::Always));
            assert_eq!(remember, None);
        }
        other => panic!("expected SetVerdict, got {other:?}"),
    }
}

#[test]
fn decode_client_parses_subscribe_and_rule_change_json() {
    match decode_client(r#"{"action":"subscribeBlocklist","url":"https://x/h"}"#).unwrap() {
        ClientMessage::SubscribeBlocklist { url } => assert_eq!(url, "https://x/h"),
        other => panic!("expected SubscribeBlocklist, got {other:?}"),
    }
    match decode_client(r#"{"action":"deleteRule","ruleId":"block-ads"}"#).unwrap() {
        ClientMessage::DeleteRule {
            rule_id,
            request_id: None,
            reply: None,
        } => assert_eq!(rule_id, "block-ads"),
        other => panic!("expected DeleteRule, got {other:?}"),
    }
}

#[test]
fn decode_client_parses_profile_json() {
    match decode_client(
        r#"{"action":"createProfile","id":"home","name":"At Home","networkMatchers":["Home*"]}"#,
    )
    .unwrap()
    {
        ClientMessage::CreateProfile {
            id,
            name,
            network_matchers,
            ..
        } => {
            assert_eq!(id, "home");
            assert_eq!(name, "At Home");
            assert_eq!(network_matchers, vec!["Home*".to_string()]);
        }
        other => panic!("expected CreateProfile, got {other:?}"),
    }
    match decode_client(r#"{"action":"activateProfile","id":"home"}"#).unwrap() {
        ClientMessage::ActivateProfile { id } => assert_eq!(id, "home"),
        other => panic!("expected ActivateProfile, got {other:?}"),
    }
    match decode_client(r#"{"action":"deactivateProfile"}"#).unwrap() {
        ClientMessage::DeactivateProfile => {}
        other => panic!("expected DeactivateProfile, got {other:?}"),
    }
}

#[test]
fn decode_client_rejects_malformed_json() {
    assert!(decode_client("{ not json").is_err());
    assert!(decode_client(r#"{"action":"noSuchAction"}"#).is_err());
}

#[tokio::test]
async fn run_feed_delivers_only_messages_its_predicate_routes() {
    let (btx, brx) = tokio::sync::broadcast::channel(16);
    let (dtx, mut drx) = tokio::sync::mpsc::unbounded_channel();

    let feed = tokio::spawn(run_feed(
        brx,
        || Ok(()),
        "test-rules",
        interests_rules,
        move |connection_id, msg, json| {
            assert_eq!(connection_id, 7, "feed preserves the source session");
            assert!(
                interests_rules(msg),
                "deliver must only see routed messages"
            );
            let _ = dtx.send(json);
        },
    ));

    btx.send(ReceivedServerMessage {
        connection_id: 7,
        message: ServerMessage::SetRules { rules: vec![] },
    })
    .unwrap();
    btx.send(ReceivedServerMessage {
        connection_id: 7,
        message: ServerMessage::ClearConnectionRows,
    })
    .unwrap(); // filtered out
    btx.send(ReceivedServerMessage {
        connection_id: 7,
        message: ServerMessage::UpdateRules { rules: vec![] },
    })
    .unwrap();

    let first = tokio::time::timeout(std::time::Duration::from_secs(1), drx.recv())
        .await
        .expect("no delivery")
        .expect("deliver channel closed");
    assert!(
        first.contains("etRules"),
        "expected a rules message, got {first}"
    );
    let second = tokio::time::timeout(std::time::Duration::from_secs(1), drx.recv())
        .await
        .expect("no second delivery")
        .expect("deliver channel closed");
    assert!(
        second.contains("Rules"),
        "expected a rules message, got {second}"
    );

    drop(btx); // Closed → loop exits
    tokio::time::timeout(std::time::Duration::from_secs(1), feed)
        .await
        .expect("feed did not exit on channel close")
        .unwrap();
}

#[tokio::test]
async fn run_feed_requests_snapshot_resync_after_lagging() {
    // Capacity-1 broadcast: overrunning it without yielding guarantees
    // the next recv() observes Lagged.
    let (btx, brx) = tokio::sync::broadcast::channel(1);
    let (itx, mut irx) = tokio::sync::mpsc::channel(4);

    let feed = tokio::spawn(run_feed(
        brx,
        move || {
            itx.try_send(ClientMessage::RequestSnapshot)
                .map_err(|_| SendClientMessageError::Full)
        },
        "test-lag",
        interests_connections,
        |_connection_id, _msg, _json| {},
    ));

    assert_eq!(irx.recv().await.unwrap(), ClientMessage::RequestSnapshot);
    for _ in 0..3 {
        btx.send(ReceivedServerMessage {
            connection_id: 1,
            message: ServerMessage::ClearConnectionRows,
        })
        .unwrap();
    }
    let resync = tokio::time::timeout(std::time::Duration::from_secs(1), irx.recv())
        .await
        .expect("no snapshot request after lag")
        .expect("inbound channel closed");
    assert_eq!(resync, ClientMessage::RequestSnapshot);
    drop(btx);
    feed.await.unwrap();
}

#[tokio::test]
async fn run_feed_retries_initial_snapshot_when_disconnected() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (btx, brx) = tokio::sync::broadcast::channel(16);
    let connected = Arc::new(AtomicBool::new(false));
    let request_connected = connected.clone();
    let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
    let (dtx, mut drx) = tokio::sync::mpsc::unbounded_channel();
    let feed = tokio::spawn(run_feed(
        brx,
        move || {
            let result = if request_connected.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(SendClientMessageError::Disconnected)
            };
            atx.send(result).unwrap();
            result
        },
        "test-reconnect",
        interests_connections,
        move |session, _, _| {
            dtx.send(session).unwrap();
        },
    ));
    assert_eq!(
        arx.recv().await.unwrap(),
        Err(SendClientMessageError::Disconnected)
    );
    connected.store(true, Ordering::SeqCst);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), arx.recv())
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    btx.send(ReceivedServerMessage {
        connection_id: 2,
        message: ServerMessage::ClearConnectionRows,
    })
    .unwrap();
    assert_eq!(drx.recv().await.unwrap(), 2);
    drop(btx);
    feed.await.unwrap();
}

#[tokio::test]
async fn run_feed_survives_transient_snapshot_rejection_after_lag() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for failure in [
        SendClientMessageError::Full,
        SendClientMessageError::Disconnected,
    ] {
        let (btx, brx) = tokio::sync::broadcast::channel(1);
        let (atx, mut arx) = tokio::sync::mpsc::unbounded_channel();
        let (dtx, mut drx) = tokio::sync::mpsc::unbounded_channel();
        let attempts = AtomicUsize::new(0);
        let feed = tokio::spawn(run_feed(
            brx,
            move || {
                let result = if attempts.fetch_add(1, Ordering::SeqCst) == 1 {
                    Err(failure)
                } else {
                    Ok(())
                };
                atx.send(result).unwrap();
                result
            },
            "test-transient-lag",
            interests_connections,
            move |session, _, _| {
                dtx.send(session).unwrap();
            },
        ));
        assert_eq!(arx.recv().await.unwrap(), Ok(()));
        // No await between sends: the single-thread test executor cannot
        // poll the feed before the buffer overruns.
        for _ in 0..3 {
            btx.send(ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::ClearConnectionRows,
            })
            .unwrap();
        }
        assert_eq!(arx.recv().await.unwrap(), Err(failure));
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), arx.recv())
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
        while drx.try_recv().is_ok() {}
        btx.send(ReceivedServerMessage {
            connection_id: 2,
            message: ServerMessage::ClearConnectionRows,
        })
        .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), drx.recv())
                .await
                .unwrap()
                .unwrap(),
            2
        );
        drop(btx);
        feed.await.unwrap();
    }
}

#[tokio::test]
async fn run_feed_exits_when_snapshot_client_has_stopped() {
    let (_btx, brx) = tokio::sync::broadcast::channel(1);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        run_feed(
            brx,
            || Err(SendClientMessageError::Stopped),
            "test-stopped",
            interests_connections,
            |_, _, _| {},
        ),
    )
    .await
    .expect("stopped client must release its feed");
}
