use super::*;

fn set_filtering_paused(paused: bool, duration_secs: Option<u64>) -> ClientMessage {
    ClientMessage::SetFilteringPaused {
        paused,
        duration_secs,
        sender_generation: None,
        sender_uid: None,
    }
}

#[test]
fn client_set_filtering_paused_round_trips() {
    let msg = set_filtering_paused(true, Some(1800));
    let json = serde_json::to_string(&msg).unwrap();
    assert_eq!(
        json,
        r#"{"action":"setFilteringPaused","paused":true,"durationSecs":1800}"#
    );
    assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), msg);

    let resume = set_filtering_paused(false, None);
    let json = serde_json::to_string(&resume).unwrap();
    assert_eq!(json, r#"{"action":"setFilteringPaused","paused":false}"#);
    assert_eq!(
        serde_json::from_str::<ClientMessage>(&json).unwrap(),
        resume
    );
}

#[test]
fn legacy_pause_without_a_duration_still_parses() {
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"action":"setFilteringPaused","paused":true}"#)
            .unwrap(),
        set_filtering_paused(true, None)
    );
}

#[test]
fn a_client_cannot_supply_the_sender_generation() {
    let parsed: ClientMessage = serde_json::from_str(
        r#"{"action":"setFilteringPaused","paused":true,"durationSecs":300,"senderGeneration":7,"senderUid":0}"#,
    )
    .unwrap();
    assert_eq!(parsed, set_filtering_paused(true, Some(300)));

    let stamped = ClientMessage::SetFilteringPaused {
        paused: true,
        duration_secs: Some(300),
        sender_generation: Some(7),
        sender_uid: Some(0),
    };
    let json = serde_json::to_string(&stamped).unwrap();
    assert!(
        !json.contains("sender"),
        "stamp leaked onto the wire: {json}"
    );
}

/// P2.6 Part 1: the largest `RuleHits` the bridge can send (every rule
/// it tracks, each name at the length limit and made of characters JSON
/// doubles, the widest numbers, a long storage reason) fits one frame of
/// a GUI client with tungstenite's default limit (the Kirigami shell's
/// `client_async` uses the default config).
#[test]
fn the_largest_rule_hits_fits_a_gui_clients_frame() {
    use crate::cache::rule_hits::{MAX_HIT_NAME_BYTES, MAX_TRACKED_RULES};
    let limit = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size
        .expect("tungstenite's default has a frame limit");
    let hits = (0..MAX_TRACKED_RULES)
        .map(|i| {
            let stem = format!("{i:05}");
            let fill = "\"\\".repeat((MAX_HIT_NAME_BYTES - stem.len()) / 2);
            RuleHitWire {
                name: format!("{stem}{fill}"),
                count: u64::MAX,
                last_hit_unix_ms: i64::MIN,
            }
        })
        .collect();
    let msg = ServerMessage::RuleHits {
        since_unix_ms: Some(i64::MIN),
        lossy: true,
        last_gap_unix_ms: Some(i64::MIN),
        storage: StorageStatus {
            persistent: false,
            // Two paths at PATH_MAX and an OS error, all escaped.
            reason: Some("\"".repeat(16 * 1024)),
            unreadable: false,
        },
        hits,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.len() < limit, "{} bytes, limit {limit}", json.len());
}

#[test]
fn rule_hits_round_trips_with_camel_case_keys() {
    let message = ServerMessage::RuleHits {
        since_unix_ms: Some(1_800_000_000_000),
        lossy: true,
        last_gap_unix_ms: Some(1_800_000_100_000),
        storage: StorageStatus {
            persistent: false,
            reason: Some("no state directory".into()),
            unreadable: false,
        },
        hits: vec![RuleHitWire {
            name: "000-allow-curl".into(),
            count: 7,
            last_hit_unix_ms: 1_800_000_050_000,
        }],
    };
    let json = serde_json::to_value(&message).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "action": "ruleHits",
            "sinceUnixMs": 1_800_000_000_000_i64,
            "lossy": true,
            "lastGapUnixMs": 1_800_000_100_000_i64,
            "storage": {"persistent": false, "reason": "no state directory"},
            "hits": [{"name": "000-allow-curl", "count": 7, "lastHitUnixMs": 1_800_000_050_000_i64}],
        })
    );
    assert_eq!(
        serde_json::from_value::<ServerMessage>(json).unwrap(),
        message
    );
}

#[test]
fn filter_pause_state_round_trips() {
    let paused = ServerMessage::FilterPauseState {
        paused: true,
        expires_at_unix_ms: Some(1_800_000_300_000),
    };
    let json = serde_json::to_string(&paused).unwrap();
    assert_eq!(
        json,
        r#"{"action":"filterPauseState","paused":true,"expiresAtUnixMs":1800000300000}"#
    );
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&json).unwrap(),
        paused
    );

    let not_paused = ServerMessage::FilterPauseState {
        paused: false,
        expires_at_unix_ms: None,
    };
    let json = serde_json::to_string(&not_paused).unwrap();
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&json).unwrap(),
        not_paused
    );
}

#[test]
fn diagnostics_report_round_trips() {
    let msg = ServerMessage::DiagnosticsReport {
        checks: vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::EbpfSupport,
                status: CheckStatus::Failed {
                    detail: "no BTF".to_string(),
                },
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: CheckStatus::Unknown,
            },
        ],
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"action\":\"diagnosticsReport\""));
    let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, msg);
}

#[test]
fn recheck_diagnostics_round_trips() {
    let msg = ClientMessage::RecheckDiagnostics;
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"action\":\"recheckDiagnostics\""));
    let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, msg);
}
