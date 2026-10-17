use std::collections::HashSet;

use super::*;

#[test]
fn grouped_flush_discards_disconnected_and_replaced_sessions() {
    let buffer = || {
        vec![
            BufferedConnectionMessage {
                connection_id: Some(1),
                message: ServerMessage::ClearConnectionRows,
            },
            BufferedConnectionMessage {
                connection_id: Some(2),
                message: ServerMessage::InsertConnectionRows {
                    rows: vec![row("2:1", None)],
                },
            },
        ]
    };
    assert!(take_current_grouped_messages(buffer(), |_| false).is_empty());
    let current = take_current_grouped_messages(buffer(), |id| id == 2);
    assert_eq!(current.len(), 1);
    assert!(
        matches!(&current[0], ServerMessage::InsertConnectionRows { rows } if rows[0].id == "2:1")
    );
}

#[test]
fn row_identity_and_all_connection_deltas_preserve_source_session() {
    let mut insert = ServerMessage::InsertConnectionRows {
        rows: vec![row("1", None)],
    };
    qualify_connection_ids(&mut insert, 7);
    let mut store = RowStore::default();
    store.apply(insert);
    assert!(store.row_by_id("7:1").is_some());
    assert!(store.row_by_id("1").is_none());
    let mut update = ServerMessage::UpdateConnectionRows {
        rows: vec![row("1", Some("allow"))],
    };
    qualify_connection_ids(&mut update, 7);
    store.apply(update);
    assert_eq!(
        store.row_by_id("7:1").unwrap().action.as_deref(),
        Some("allow")
    );
    let mut moved = ServerMessage::MoveConnetionRows {
        ids: vec!["1".into()],
    };
    qualify_connection_ids(&mut moved, 7);
    assert!(matches!(&moved, ServerMessage::MoveConnetionRows { ids } if ids == &["7:1"]));
    let mut remove = ServerMessage::RemoveConnectionRows {
        ids: vec!["1".into()],
    };
    qualify_connection_ids(&mut remove, 7);
    store.apply(remove);
    assert!(store.row_by_id("7:1").is_none());
}

fn row(id: &str, action: Option<&str>) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: "p".into(),
        process_path: None,
        dst_host: "h".into(),
        dst_ip: "1.1.1.1".into(),
        dst_port: 1,
        protocol: "tcp".into(),
        direction: "outgoing".into(),
        action: action.map(str::to_string),
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
fn oldest_pending_position_picks_smallest_start_topmost_on_ties() {
    let mut a = row("a", None);
    a.started_at_ms = 5;
    let mut b = row("b", None);
    b.started_at_ms = 2;
    let mut c = row("c", None);
    c.started_at_ms = 2;
    let mut d = row("d", Some("allow"));
    d.started_at_ms = 1;
    let list = [None, Some(&a), Some(&b), Some(&c), Some(&d)];
    let none: HashSet<String> = HashSet::new();
    let first = |_: &str| 0;
    assert_eq!(
        oldest_pending_position(list.iter().copied(), &none, first),
        Some(2)
    );
    let except_b: HashSet<String> = ["b".to_string()].into();
    assert_eq!(
        oldest_pending_position(list.iter().copied(), &except_b, first),
        Some(3)
    );
    assert_eq!(
        oldest_pending_position([None, Some(&d)].into_iter(), &none, first),
        None
    );
}

/// Issue #146: every dismissed row is skipped, not just the last.
#[test]
fn oldest_pending_position_skips_every_excepted_row() {
    let rows: Vec<ConnectionRow> = (0..4)
        .map(|i| {
            let mut r = row(&format!("p{i}"), None);
            r.started_at_ms = i;
            r
        })
        .collect();
    let except: HashSet<String> = ["p0".to_string(), "p1".to_string()].into();
    assert_eq!(
        oldest_pending_position(rows.iter().map(Some), &except, |_| 0),
        Some(2)
    );
    let all: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(
        oldest_pending_position(rows.iter().map(Some), &all, |_| 0),
        None
    );
}

/// Issue #146 tie-break: equal starts go to the row earlier in the store
/// (the rule `alert_row` uses), not the topmost drawn one. Grouped mode
/// draws store order A, B, C as A, C, B.
#[test]
fn oldest_pending_position_breaks_ties_by_store_order() {
    let (a, b, c) = (row("a", None), row("b", None), row("c", None));
    let drawn = [None, Some(&a), Some(&c), None, Some(&b)];
    let store_order = |id: &str| ["a", "b", "c"].iter().position(|s| *s == id).unwrap();
    let except_a: HashSet<String> = ["a".to_string()].into();
    assert_eq!(
        oldest_pending_position(drawn.iter().copied(), &except_a, store_order),
        Some(4),
        "b (second in the store) must win over c (drawn first)"
    );
}

#[test]
fn except_ids_parse_from_a_json_array_and_fall_back_to_none() {
    assert_eq!(
        parse_except_ids(r#"["a","b"]"#),
        ["a".to_string(), "b".to_string()].into()
    );
    assert!(parse_except_ids("").is_empty());
    assert!(parse_except_ids("not json").is_empty());
    assert!(parse_except_ids(r#"{"a":1}"#).is_empty());
}

#[test]
fn pending_age_secs_is_sentinel_when_nothing_pending() {
    assert_eq!(pending_age_secs(None, 10_000), -1);
}

#[test]
fn pending_age_secs_computes_elapsed_seconds() {
    assert_eq!(pending_age_secs(Some(1_000), 14_500), 13);
}

#[test]
fn pending_age_secs_saturates_at_zero_for_clock_skew() {
    // started_at_ms ahead of now_ms (e.g. clock skew) must never go negative.
    assert_eq!(pending_age_secs(Some(5_000), 1_000), 0);
}

#[test]
fn collect_new_pending_ids_spans_the_whole_batch() {
    let msgs = vec![
        ServerMessage::InsertConnectionRows {
            rows: vec![row("a", None), row("b", Some("allow"))],
        },
        ServerMessage::ClearConnectionRows,
        ServerMessage::InsertConnectionRows {
            rows: vec![row("c", None)],
        },
        // Updates never count as *new* pending arrivals.
        ServerMessage::UpdateConnectionRows {
            rows: vec![row("d", None)],
        },
    ];
    assert_eq!(collect_new_pending_ids(&msgs), vec!["a", "c"]);
}

#[test]
fn collect_new_pending_ids_empty_for_non_insert_batches() {
    assert!(collect_new_pending_ids(&[ServerMessage::ClearConnectionRows]).is_empty());
    assert!(collect_new_pending_ids(&[]).is_empty());
}

#[test]
fn new_row_helpers_read_the_bridge_row() {
    let mut r = row("a", None);
    assert_eq!(process_path_text(&r), "");
    r.process_path = Some("/usr/bin/curl".into());
    r.started_at_ms = 1_760_000_000_123;
    assert_eq!(process_path_text(&r), "/usr/bin/curl");
    assert_eq!(started_at_ms(&r), 1_760_000_000_123.0);
}

#[test]
fn app_icon_is_generic_without_a_process_path() {
    assert_eq!(
        app_icon_for_row(&row("a", None)),
        crate::app_icons::FALLBACK_ICON
    );
}

#[test]
fn a_silently_answered_row_needs_a_rule_once_it_is_decided() {
    let mut answered = row("7:silent-answered", None);
    assert!(!needs_rule_for_row(&answered), "unmarked");
    crate::silent_mode::mark_needs_rule("7:silent-answered");
    assert!(!needs_rule_for_row(&answered), "still waiting");
    answered.action = Some("allow".into());
    assert!(needs_rule_for_row(&answered));
    assert!(!needs_rule_for_row(&row("7:other", Some("allow"))));
}

#[test]
fn group_level_reports_program_domain_host_connection() {
    use crate::connections::grouping::GroupCounts;
    let store = RowStore::default();
    let counts = GroupCounts::default();
    let level =
        |entry: &VisibleEntry| grouped_entry_data(entry, ROLE_GROUP_LEVEL, &store).value::<i32>();
    let program = VisibleEntry::ProcessHeader {
        key: "p".into(),
        label: "p".into(),
        expanded: true,
        counts,
    };
    let domain = VisibleEntry::DomainHeader {
        process_key: "p".into(),
        key: "d.com".into(),
        label: "d.com".into(),
        expanded: true,
        counts,
    };
    let host = VisibleEntry::HostHeader {
        process_key: "p".into(),
        domain_key: "d.com".into(),
        key: "h.d.com".into(),
        label: "h.d.com".into(),
        expanded: true,
        counts,
    };
    let conn = VisibleEntry::Row {
        id: "1".into(),
        process_key: "p".into(),
        domain_key: "d.com".into(),
        host_key: Some("h.d.com".into()),
    };
    assert_eq!(
        [&program, &domain, &host, &conn].map(level),
        [Some(0), Some(1), Some(2), Some(3)]
    );
    let process_key =
        |e: &VisibleEntry| grouped_entry_data(e, ROLE_GROUP_PROCESS_KEY, &store).value::<QString>();
    assert_eq!(process_key(&host).map(|k| k.to_string()), Some("p".into()));
    assert_eq!(process_key(&domain).map(|k| k.to_string()), Some("".into()));
}

#[test]
fn new_roles_have_distinct_ids_and_names() {
    let ids = [
        ROLE_PROCESS_PATH,
        ROLE_DIRECTION,
        ROLE_STARTED_AT_MS,
        ROLE_APP_ICON,
        ROLE_NEEDS_RULE,
        ROLE_GROUP_LEVEL,
        ROLE_GROUP_PROCESS_KEY,
        ROLE_LEVEL_RULE_STATE,
        ROLE_LEVEL_RULE_NAME,
    ];
    for (i, a) in ids.iter().enumerate() {
        assert!(*a > ROLE_MAKE_RULE_OFFERED);
        assert!(!ids[i + 1..].contains(a));
    }
}
