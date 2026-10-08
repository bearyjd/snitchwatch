use super::*;

#[test]
fn set_blocklists_serializes_to_camel_case_action() {
    let msg = ServerMessage::SetBlocklists {
        blocklists: vec![BlocklistSummary {
            id: "stevenblack".into(),
            display_name: "StevenBlack".into(),
            url: "https://x.example/hosts".into(),
            entry_count: 1234,
            status: "ok".into(),
            last_updated_iso8601: Some("2026-04-11T12:00:00Z".into()),
            last_failure_reason: None,
            enforcement: ENFORCEMENT_NOT_ENFORCED.into(),
            enforcement_reason: Some("no rule sink yet".into()),
        }],
        storage: Some(StorageStatus {
            unreadable: false,
            persistent: false,
            reason: Some("blocklist store: disk full".into()),
        }),
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["action"], "setBlocklists");
    assert_eq!(json["blocklists"][0]["id"], "stevenblack");
    assert_eq!(json["blocklists"][0]["displayName"], "StevenBlack");
    assert_eq!(json["blocklists"][0]["entryCount"], 1234);
    assert_eq!(json["blocklists"][0]["enforcement"], "not_enforced");
    assert_eq!(
        json["blocklists"][0]["enforcementReason"],
        "no rule sink yet"
    );
    assert_eq!(json["storage"]["persistent"], false);
    assert_eq!(json["storage"]["reason"], "blocklist store: disk full");
}

/// An older bridge sends neither `storage` nor the enforcement fields.
#[test]
fn set_blocklists_from_an_older_bridge_still_parses() {
    let json = r#"{"action":"setBlocklists","blocklists":[{"id":"a","displayName":"A",
        "url":"https://x.example/a","entryCount":1,"status":"ok"}]}"#;
    match serde_json::from_str::<ServerMessage>(json).unwrap() {
        ServerMessage::SetBlocklists {
            blocklists,
            storage,
        } => {
            assert_eq!(storage, None);
            assert_eq!(blocklists[0].enforcement, "");
            assert_eq!(blocklists[0].enforcement_reason, None);
        }
        other => panic!("expected SetBlocklists, got {other:?}"),
    }
}

#[test]
fn leftover_blocklist_rules_are_counted_and_removed_by_additive_messages() {
    let msg = ServerMessage::SetBlocklistLeftovers { count: 3 };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"action": "setBlocklistLeftovers", "count": 3})
    );
    assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), msg);
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"action":"removeLeftoverBlocklistRules"}"#)
            .unwrap(),
        ClientMessage::RemoveLeftoverBlocklistRules
    );
}

#[test]
fn set_blocklist_entries_carries_strongly_typed_entries() {
    let msg = ServerMessage::SetBlocklistEntries {
        offset: 0,
        total: 2,
        request_id: None,
        last_updated_iso8601: None,
        subscription_id: "stevenblack".into(),
        entries: vec![
            BlocklistEntry {
                host: "doubleclick.net".into(),
            },
            BlocklistEntry {
                host: "google-analytics.com".into(),
            },
        ],
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["action"], "setBlocklistEntries");
    assert_eq!(json["subscriptionId"], "stevenblack");
    assert_eq!(json["entries"][0]["host"], "doubleclick.net");
    // Additive fields stay off the wire when empty, so an older GUI sees
    // exactly the page it always did.
    assert!(json.get("requestId").is_none());
    assert!(json.get("lastUpdatedIso8601").is_none());
}

/// Issue #67: a page names the request it answers and the download it was
/// read from, and an older bridge's page (neither) still parses.
#[test]
fn an_entries_page_echoes_its_request_and_names_its_download() {
    let msg = ServerMessage::SetBlocklistEntries {
        subscription_id: "a".into(),
        entries: vec![],
        offset: 1000,
        total: 2500,
        request_id: Some("gui-1".into()),
        last_updated_iso8601: Some("2026-10-08T12:00:00Z".into()),
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["requestId"], "gui-1");
    assert_eq!(json["lastUpdatedIso8601"], "2026-10-08T12:00:00Z");
    assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), msg);

    let older = r#"{"action":"setBlocklistEntries","subscriptionId":"a","entries":[]}"#;
    match serde_json::from_str::<ServerMessage>(older).unwrap() {
        ServerMessage::SetBlocklistEntries {
            request_id,
            last_updated_iso8601,
            ..
        } => assert_eq!((request_id, last_updated_iso8601), (None, None)),
        other => panic!("expected SetBlocklistEntries, got {other:?}"),
    }
}

/// Issue #45 (S2): the largest possible entries page (every host at the
/// 253-byte maximum) stays far below a GUI client's 16 MiB frame limit.
#[test]
fn the_largest_entries_page_fits_one_small_frame() {
    let host = format!("{}.example", "a".repeat(245));
    let msg = ServerMessage::SetBlocklistEntries {
        subscription_id: "x".repeat(81),
        entries: (0..BLOCKLIST_ENTRIES_PAGE_MAX)
            .map(|_| BlocklistEntry { host: host.clone() })
            .collect(),
        offset: u64::MAX,
        total: u64::MAX,
        request_id: Some("r".repeat(MAX_REQUEST_ID_LEN)),
        last_updated_iso8601: Some("2026-10-08T12:00:00.123456789+00:00".into()),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.len() < 1024 * 1024, "{} bytes", json.len());
}

#[test]
fn request_blocklist_entries_parses_with_defaults() {
    let parsed: ClientMessage =
        serde_json::from_str(r#"{"action":"requestBlocklistEntries","subscriptionId":"a"}"#)
            .unwrap();
    assert_eq!(
        parsed,
        ClientMessage::RequestBlocklistEntries {
            subscription_id: "a".into(),
            offset: 0,
            limit: None,
            request_id: None,
        }
    );
    let tagged: ClientMessage = serde_json::from_str(
        r#"{"action":"requestBlocklistEntries","subscriptionId":"a","requestId":"gui-1"}"#,
    )
    .unwrap();
    assert!(matches!(
        tagged,
        ClientMessage::RequestBlocklistEntries { request_id: Some(ref id), .. } if id == "gui-1"
    ));
}

#[test]
fn subscribe_blocklist_action_round_trips() {
    let action = ClientMessage::SubscribeBlocklist {
        url: "https://x.example/hosts".into(),
    };
    let json = serde_json::to_string(&action).unwrap();
    let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, action);
}
