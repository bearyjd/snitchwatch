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
            decided_by_default: false,
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
        decided_by_default: false,
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
    assert!(!parsed.decided_by_default);
}

/// E3 (plan `2026-10-08-default-applied-events.md`): additive like
/// `deferred`, absent unless set.
#[test]
fn decided_by_default_is_sent_only_when_set_and_has_no_rule() {
    let row = ConnectionRow {
        id: "event-1".to_string(),
        process: "curl".to_string(),
        process_path: Some("/usr/bin/curl".to_string()),
        dst_host: "example.com".to_string(),
        dst_ip: "93.184.216.34".to_string(),
        dst_port: 443,
        protocol: "tcp".to_string(),
        direction: "outgoing".to_string(),
        action: Some("deny".to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 1,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: true,
    };
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json["decidedByDefault"], true);
    assert!(json.get("matchedRule").is_none(), "{json}");
    let parsed: ConnectionRow = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, row);

    let plain = ConnectionRow {
        decided_by_default: false,
        ..row
    };
    let json = serde_json::to_value(&plain).unwrap();
    assert!(json.get("decidedByDefault").is_none(), "{json}");
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

#[test]
fn modern_intents_reject_duplicate_unknown_and_spoofed_context_fields() {
    let good = r#"{"action":"prepareCuratedBroadOptIn","requestId":"safe-1","id":"dns-resolved","purpose":"enable"}"#;
    let ClientMessage::PrepareCuratedBroadOptIn { context, reply, .. } =
        decode_client_message(good).unwrap()
    else {
        panic!()
    };
    assert!(context.is_none() && reply.is_none());
    for extra in [
        r#", "id":"dns-resolved""#,
        r#", "purpose":"keep""#,
        r#", "action":"setCuratedDefaults""#,
        r#", "context":{"alive":true}"#,
        r#", "reply":null"#,
        r#", "ids":["dns-resolved"]"#,
        r#", "policy":{}"#,
    ] {
        let malformed = format!("{}{}{}", &good[..good.len() - 1], extra, "}");
        let failure = decode_client_message(&malformed).unwrap_err();
        assert_eq!(failure.request_id.as_deref(), Some("safe-1"));
        assert!(serde_json::from_str::<ClientMessage>(&malformed).is_err());
    }
    let duplicate_request = good.replace(
        "\"requestId\":\"safe-1\"",
        "\"requestId\":\"safe-1\",\"requestId\":\"safe-1\"",
    );
    assert_eq!(
        decode_client_message(&duplicate_request)
            .unwrap_err()
            .request_id,
        None
    );
}

#[test]
fn modern_intent_bounds_shape_purpose_and_safe_correlation() {
    let ticket = "a".repeat(64);
    let good = serde_json::json!({"action":"enableCuratedBroadDefault","requestId":"safe-1","id":"dns-resolved","purpose":"reenable","ticket":ticket,"consent":"resolver-any-address-v1"});
    let message = decode_client_message(&good.to_string()).unwrap();
    assert!(!format!("{message:?}").contains(&ticket));
    for (field, wrong) in [
        ("requestId", serde_json::json!("x".repeat(65))),
        ("requestId", serde_json::json!("not_safe")),
        ("requestId", serde_json::json!(1)),
        ("id", serde_json::json!(["dns-resolved"])),
        ("id", serde_json::json!("unknown")),
        ("purpose", serde_json::json!("keep")),
        ("ticket", serde_json::json!("A".repeat(64))),
        ("ticket", serde_json::json!("a".repeat(63))),
        ("consent", serde_json::json!("wrong")),
    ] {
        let mut value = good.clone();
        value[field] = wrong;
        assert!(decode_client_message(&value.to_string()).is_err());
        assert!(serde_json::from_value::<ClientMessage>(value).is_err());
    }
    for missing in ["requestId", "id", "purpose", "ticket", "consent"] {
        let mut value = good.clone();
        value.as_object_mut().unwrap().remove(missing);
        assert!(decode_client_message(&value.to_string()).is_err());
    }
    assert!(decode_client_message(&format!("{}{}", " ".repeat(4096), good)).is_err());
    let prepared = ServerMessage::CuratedBroadOptInPrepared {
        request_id: "safe-1".into(),
        id: "dns-resolved".into(),
        purpose: CuratedIntentPurpose::Enable,
        ticket: ticket.clone(),
    };
    assert!(!format!("{prepared:?}").contains(&ticket));
    assert_eq!(serde_json::to_value(prepared).unwrap()["ticket"], ticket);
}

#[test]
fn legacy_curated_json_stays_compatible_and_defaults_private_reply_absent() {
    let ClientMessage::SetCuratedDefaults { ids, on, request_id, reply } = decode_client_message(r#"{"action":"setCuratedDefaults","ids":["dns-resolved"],"on":true,"ignoredOldExtension":1}"#).unwrap() else { panic!() };
    assert_eq!(ids, vec!["dns-resolved"]);
    assert!(on && request_id.is_none() && reply.is_none());
    assert!(decode_client_message(
        r#"{"action":"setCuratedDefaults","ids":[],"on":true,"on":false}"#
    )
    .is_err());
}

#[test]
fn curated_groups_round_trip_and_an_older_message_without_them_still_parses() {
    let storage = crate::ws_messages::StorageStatus {
        persistent: true,
        reason: None,
        unreadable: false,
    };
    let group = crate::curated::CuratedGroup {
        id: "tailscale".into(),
        title: "Tailscale".into(),
        description: "d".into(),
    };
    let with = ServerMessage::SetCuratedDefaults {
        entries: Vec::new(),
        groups: vec![group.clone()],
        orphans: Vec::new(),
        storage: storage.clone(),
        unavailable: None,
    };
    let wire = serde_json::to_value(&with).unwrap();
    assert_eq!(wire["groups"][0]["id"], "tailscale");
    assert_eq!(wire["groups"][0]["title"], "Tailscale");
    assert_eq!(serde_json::from_value::<ServerMessage>(wire).unwrap(), with);
    // No groups: the field is left out, so an older GUI sees the old shape.
    let plain = serde_json::to_value(ServerMessage::SetCuratedDefaults {
        entries: Vec::new(),
        groups: Vec::new(),
        orphans: Vec::new(),
        storage,
        unavailable: None,
    })
    .unwrap();
    assert!(plain.get("groups").is_none(), "{plain}");
    // An older bridge's message has no `groups` at all.
    let old = r#"{"action":"setCuratedDefaults","entries":[],"storage":{"persistent":true,"unreadable":false}}"#;
    let ServerMessage::SetCuratedDefaults { groups, .. } =
        serde_json::from_str::<ServerMessage>(old).unwrap()
    else {
        panic!()
    };
    assert!(groups.is_empty());
}

/// #113: the orphan list is left out when empty (and by an older bridge),
/// and round-trips when sent; the removal request is its own action.
#[test]
fn set_curated_defaults_orphans_are_optional_on_the_wire() {
    use crate::curated::wire::CuratedOrphanSummary;
    let storage = crate::ws_messages::StorageStatus {
        persistent: true,
        reason: None,
        unreadable: false,
    };
    let orphan = CuratedOrphanSummary {
        id: "retired".into(),
        enabled: true,
        file_left: false,
        problem: None,
    };
    let with = ServerMessage::SetCuratedDefaults {
        entries: Vec::new(),
        groups: Vec::new(),
        orphans: vec![orphan],
        storage: storage.clone(),
        unavailable: None,
    };
    let wire = serde_json::to_value(&with).unwrap();
    assert_eq!(wire["orphans"][0]["id"], "retired");
    assert_eq!(serde_json::from_value::<ServerMessage>(wire).unwrap(), with);
    let plain = serde_json::to_value(ServerMessage::SetCuratedDefaults {
        entries: Vec::new(),
        groups: Vec::new(),
        orphans: Vec::new(),
        storage,
        unavailable: None,
    })
    .unwrap();
    assert!(plain.get("orphans").is_none(), "{plain}");
    let old = r#"{"action":"setCuratedDefaults","entries":[],"storage":{"persistent":true,"unreadable":false}}"#;
    let ServerMessage::SetCuratedDefaults { orphans, .. } =
        serde_json::from_str::<ServerMessage>(old).unwrap()
    else {
        panic!()
    };
    assert!(orphans.is_empty());
    let ask = crate::ws_messages::decode_client_message(
        r#"{"action":"removeCuratedOrphan","id":"retired"}"#,
    )
    .unwrap();
    assert_eq!(
        ask,
        ClientMessage::RemoveCuratedOrphan {
            id: "retired".into()
        }
    );
}
