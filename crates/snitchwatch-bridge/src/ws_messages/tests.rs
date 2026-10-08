use super::*;

#[test]
fn server_message_round_trips_via_json() {
    let msg = ServerMessage::InsertConnectionRows {
        rows: vec![ConnectionRow {
            id: "r1".to_string(),
            process: "firefox".to_string(),
            process_path: Some("/usr/bin/firefox".to_string()),
            dst_host: "github.com".to_string(),
            dst_ip: "140.82.121.4".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 1_700_000_000_000,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
        }],
    };

    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""action":"insertConnectionRows""#));
    assert!(json.contains(r#""dstHost":"github.com""#));
    assert!(
        !json.contains("matchedRule"),
        "matchedRule must be omitted when None, so old web-frontend consumers are unaffected: {json}"
    );

    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, msg);
}

#[test]
fn authenticated_ack_is_a_backward_compatible_wire_extension() {
    let message = ServerMessage::Authenticated {
        capabilities: Vec::new(),
    };
    let json = serde_json::to_string(&message).unwrap();
    assert_eq!(json, r#"{"action":"authenticated"}"#);
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&json).unwrap(),
        message
    );
}

#[test]
fn deny_scope_narrowed_round_trips_via_json() {
    // Issue #14 security review round 2, HIGH: this must be a real
    // wire-protocol message the WS client actually receives, not just a
    // desktop-notification side channel.
    let msg = ServerMessage::DenyScopeNarrowed {
        row_id: "ask-7".to_string(),
        reason: "the destination host has no subdomain that can be safely wildcarded below \
                 its public suffix"
            .to_string(),
    };

    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""action":"denyScopeNarrowed""#));
    assert!(json.contains(r#""rowId":"ask-7""#));

    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, msg);
}

#[test]
fn shell_state_extensions_round_trip_via_json() {
    let tray = ServerMessage::TrayState {
        state: TrayState::Pending(3),
    };
    let notice = ServerMessage::Notice {
        notice: Notice::Pending {
            row_id: 42,
            process: "firefox".into(),
        },
    };

    for message in [tray, notice] {
        let json = serde_json::to_string(&message).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, message);
    }
}

#[test]
fn legacy_web_dispatcher_ignores_shell_only_wire_extensions() {
    // The bundled legacy web client has an explicit default branch for
    // unknown server actions. Keep the extension discriminators absent
    // from its known-action switch: authenticated native shells consume
    // them, while old web clients continue their normal message loop.
    let legacy_dispatcher = include_str!("../../../../web/js/app.js");
    assert!(legacy_dispatcher.contains("function handleServerCommand(messageArray)"));
    assert!(
        legacy_dispatcher.contains("default:\n          console.warn(\"Unknown msg from server\"")
    );

    for message in [
        ServerMessage::Authenticated {
            capabilities: crate::bridge_capabilities::advertised(),
        },
        ServerMessage::TrayState {
            state: TrayState::Idle,
        },
        ServerMessage::Notice {
            notice: Notice::DaemonAway,
        },
        ServerMessage::FilterPauseState {
            paused: false,
            expires_at_unix_ms: None,
        },
        crate::prompt_slot::PromptSlot::default().message(),
        ServerMessage::RuleHits {
            since_unix_ms: None,
            lossy: false,
            last_gap_unix_ms: None,
            storage: StorageStatus {
                persistent: false,
                reason: None,
                unreadable: false,
            },
            hits: Vec::new(),
        },
    ] {
        let action = serde_json::to_value(message).unwrap()["action"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            !legacy_dispatcher.contains(&format!("case \"{action}\"")),
            "legacy web client must treat {action} as an ignorable unknown action"
        );
    }
}

#[test]
fn connection_row_carries_matched_rule_when_decided() {
    let row = ConnectionRow {
        id: "r1".to_string(),
        process: "firefox".to_string(),
        process_path: Some("/usr/bin/firefox".to_string()),
        dst_host: "github.com".to_string(),
        dst_ip: "140.82.121.4".to_string(),
        dst_port: 443,
        protocol: "tcp".to_string(),
        direction: "outgoing".to_string(),
        action: Some("allow".to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 1_700_000_000_000,
        matched_rule: Some("899-firefox-allow-out.json".to_string()),
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
    };
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json["matchedRule"], "899-firefox-allow-out.json");

    let parsed: ConnectionRow = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, row);
}

#[test]
fn connection_row_without_matched_rule_field_defaults_to_none() {
    // Simulates an old wire payload (or a hand-authored test fixture)
    // that predates this field entirely.
    let json = serde_json::json!({
        "id": "r1",
        "process": "firefox",
        "processPath": null,
        "dstHost": "github.com",
        "dstIp": "140.82.121.4",
        "dstPort": 443,
        "protocol": "tcp",
        "direction": "outgoing",
        "action": null,
        "bytesSent": 0,
        "bytesReceived": 0,
        "startedAtMs": 0
    });
    let parsed: ConnectionRow = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.matched_rule, None);
}

#[test]
fn move_connection_rows_preserves_upstream_typo() {
    let msg = ServerMessage::MoveConnetionRows {
        ids: vec!["r1".to_string()],
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(
        json.contains(r#""action":"moveConnetionRows""#),
        "must preserve upstream LS typo: {}",
        json
    );
}

#[test]
fn client_set_verdict_parses() {
    let json = r#"{
        "action": "setVerdict",
        "rowId": "r1",
        "verdict": "allow",
        "scope": "this_host",
        "duration": "always"
    }"#;
    let parsed: ClientMessage = serde_json::from_str(json).unwrap();
    match parsed {
        ClientMessage::SetVerdict {
            row_id,
            verdict,
            scope,
            duration,
            remember,
        } => {
            assert_eq!(row_id, "r1");
            assert_eq!(verdict, VerdictAction::Allow);
            assert_eq!(scope, VerdictScope::ThisHost);
            assert_eq!(duration, Some(VerdictDuration::Always));
            assert_eq!(remember, None);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn client_set_verdict_parses_legacy_remember_shape() {
    // The pre-duration wire shape the vendored web/ frontend still sends.
    let json = r#"{
        "action": "setVerdict",
        "rowId": "r1",
        "verdict": "deny",
        "scope": "this_host",
        "remember": true
    }"#;
    let parsed: ClientMessage = serde_json::from_str(json).unwrap();
    match parsed {
        ClientMessage::SetVerdict {
            duration, remember, ..
        } => {
            assert_eq!(duration, None);
            assert_eq!(remember, Some(true));
            assert_eq!(
                effective_verdict_duration(duration, remember),
                VerdictDuration::Always
            );
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn effective_verdict_duration_folds_legacy_and_new() {
    use VerdictDuration::*;
    // Explicit duration always wins, even over remember.
    assert_eq!(
        effective_verdict_duration(Some(FiveMinutes), Some(true)),
        FiveMinutes
    );
    // Legacy remember semantics.
    assert_eq!(effective_verdict_duration(None, Some(true)), Always);
    assert_eq!(effective_verdict_duration(None, Some(false)), Once);
    // Neither present: the safe default.
    assert_eq!(effective_verdict_duration(None, None), Once);
}

#[test]
fn verdict_duration_maps_to_daemon_strings() {
    assert_eq!(VerdictDuration::Once.daemon_duration_str(), "once");
    assert_eq!(VerdictDuration::FiveMinutes.daemon_duration_str(), "5m");
    assert_eq!(
        VerdictDuration::UntilRestart.daemon_duration_str(),
        "until restart"
    );
    assert_eq!(VerdictDuration::Always.daemon_duration_str(), "always");

    assert!(!VerdictDuration::Once.remembers());
    assert!(VerdictDuration::FiveMinutes.remembers());
    assert!(VerdictDuration::UntilRestart.remembers());
    assert!(VerdictDuration::Always.remembers());
}

#[test]
fn verdict_duration_wire_tokens_are_snake_case() {
    assert_eq!(
        serde_json::to_value(VerdictDuration::FiveMinutes).unwrap(),
        "five_minutes"
    );
    assert_eq!(
        serde_json::to_value(VerdictDuration::UntilRestart).unwrap(),
        "until_restart"
    );
}
