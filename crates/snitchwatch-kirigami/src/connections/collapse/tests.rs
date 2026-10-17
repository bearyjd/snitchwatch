use super::*;

fn event(id: &str, at: i64, action: &str) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: "python3".into(),
        process_path: Some("/usr/bin/python3".into()),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "outgoing".into(),
        action: Some(action.to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: at,
        matched_rule: Some("deny-python".into()),
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

fn insert(rows: Vec<ConnectionRow>) -> ServerMessage {
    ServerMessage::InsertConnectionRows { rows }
}

/// Feed `msgs` and return the rows a plain store would hold, in order.
fn shown(c: &mut RowCollapse, msgs: Vec<ServerMessage>) -> Vec<String> {
    let mut store = super::super::row_store::RowStore::new();
    for msg in msgs {
        for out in c.rewrite(msg) {
            store.apply(out);
        }
    }
    store.rows().iter().map(|r| r.id.clone()).collect()
}

#[test]
fn event_row_ids_are_recognised_bare_and_qualified() {
    assert!(is_event_row_id("event-1-2"));
    assert!(is_event_row_id("7:event-1-2"));
    assert!(!is_event_row_id("ask-3"));
    assert!(!is_event_row_id("7:ask-3"));
    assert!(!is_event_row_id("x:event-1"));
}

#[test]
fn repeats_within_the_window_become_one_row_with_a_count() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![event("event-1", 1_000, "deny")]),
            insert(vec![event("event-2", 2_000, "deny")]),
            insert(vec![event("event-3", 3_500, "deny")]),
        ],
    );
    assert_eq!(ids, ["event-1"]);
    assert_eq!(c.repeat_count("event-1"), 3);
    assert_eq!(c.last_seen_ms("event-1"), Some(3_500));
}

#[test]
fn absorbing_a_repeat_updates_the_representative_in_place() {
    let mut c = RowCollapse::default();
    c.rewrite(insert(vec![event("event-1", 1_000, "deny")]));
    let out = c.rewrite(insert(vec![event("event-2", 2_000, "deny")]));
    assert_eq!(
        out,
        vec![ServerMessage::UpdateConnectionRows {
            rows: vec![event("event-1", 1_000, "deny")]
        }]
    );
}

#[test]
fn the_window_slides_from_the_last_occurrence_and_ends_at_ten_seconds() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![event("event-1", 0, "deny")]),
            insert(vec![event("event-2", COLLAPSE_WINDOW_MS, "deny")]),
            insert(vec![event("event-3", 2 * COLLAPSE_WINDOW_MS, "deny")]),
            insert(vec![event("event-4", 3 * COLLAPSE_WINDOW_MS + 1, "deny")]),
        ],
    );
    assert_eq!(ids, ["event-1", "event-4"]);
    assert_eq!(c.repeat_count("event-1"), 3);
    assert_eq!(c.repeat_count("event-4"), 1);
}

#[test]
fn an_older_repeat_out_of_order_joins_within_the_window_only() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![event("event-1", 50_000, "deny")]),
            insert(vec![event("event-2", 50_000 - COLLAPSE_WINDOW_MS, "deny")]),
            insert(vec![event(
                "event-3",
                50_000 - COLLAPSE_WINDOW_MS - 1,
                "deny",
            )]),
        ],
    );
    assert_eq!(ids, ["event-1", "event-3"]);
    assert_eq!(c.last_seen_ms("event-1"), Some(50_000));
}

#[test]
fn different_verdicts_never_merge() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![insert(vec![
            event("event-1", 1_000, "deny"),
            event("event-2", 1_100, "allow"),
            event("event-3", 1_200, "blocklist"),
            event("event-4", 1_300, "reject"),
            event("event-5", 1_400, "drop"),
        ])],
    );
    assert_eq!(ids.len(), 5);
}

#[test]
fn every_key_field_keeps_rows_apart() {
    let base = event("event-0", 1_000, "deny");
    let variants: [fn(&mut ConnectionRow); 11] = [
        |r| r.matched_rule = Some("other-rule".into()),
        |r| r.matched_rule = None,
        |r| r.decided_by_default = true,
        |r| r.auto_answer = Some(AutoAnswer::FilterPaused),
        |r| r.process = "curl".into(),
        |r| r.process_path = Some("/usr/bin/curl".into()),
        |r| r.dst_host = "other.example".into(),
        |r| r.dst_ip = "203.0.113.9".into(),
        |r| r.dst_port = 80,
        |r| r.protocol = "udp".into(),
        |r| r.direction = "incoming".into(),
    ];
    for (i, change) in variants.iter().enumerate() {
        let mut c = RowCollapse::default();
        let mut other = event("event-1", 1_100, "deny");
        change(&mut other);
        let ids = shown(
            &mut c,
            vec![insert(vec![base.clone()]), insert(vec![other])],
        );
        assert_eq!(ids.len(), 2, "variant {i} merged");
    }
}

#[test]
fn pending_deferred_and_ask_rows_never_take_part() {
    let mut c = RowCollapse::default();
    let mut pending = event("event-p", 1_100, "deny");
    pending.action = None;
    let mut deferred = event("event-d", 1_200, "deny");
    deferred.deferred = true;
    let ask = event("ask-7", 1_300, "deny");
    let ask_q = event("3:ask-8", 1_400, "deny");
    let ids = shown(
        &mut c,
        vec![
            insert(vec![event("event-1", 1_000, "deny")]),
            insert(vec![pending, deferred, ask, ask_q]),
            insert(vec![event("event-2", 1_500, "deny")]),
        ],
    );
    assert_eq!(
        ids,
        ["event-1", "event-p", "event-d", "ask-7", "3:ask-8"],
        "only event-2 joins event-1"
    );
    assert_eq!(c.repeat_count("event-1"), 2);
    assert_eq!(c.repeat_count("ask-7"), 1);
}

const ANSWER_RULE: &str = "snitchwatch-allow-example.com-443-ppython3";

fn ask(id: &str, at: i64, action: Option<&str>) -> ConnectionRow {
    ConnectionRow {
        action: action.map(str::to_string),
        matched_rule: action.map(|_| ANSWER_RULE.to_string()),
        ..event(id, at, "allow")
    }
}

fn answer_event(id: &str, at: i64) -> ConnectionRow {
    ConnectionRow {
        matched_rule: Some(ANSWER_RULE.into()),
        ..event(id, at, "allow")
    }
}

fn decide(id: &str, at: i64) -> ServerMessage {
    ServerMessage::UpdateConnectionRows {
        rows: vec![ask(id, at, Some("allow"))],
    }
}

/// #114.3: the event the daemon reports for a remembered answer folds
/// into the answered Ask row and is counted there.
#[test]
fn a_remembered_answer_twin_folds_into_its_ask_row() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![ask("ask-1", 1_000, None)]),
            decide("ask-1", 1_000),
            insert(vec![answer_event("event-2", 1_000 + TWIN_WINDOW_MS)]),
        ],
    );
    assert_eq!(ids, ["ask-1"]);
    assert_eq!(c.repeat_count("ask-1"), 2);
    assert_eq!(c.last_seen_ms("ask-1"), Some(1_000 + TWIN_WINDOW_MS));
}

/// #114.3, no twin ever arrives: the app's next genuine connection under
/// the rule is the one folded; it is counted, never lost; at most one
/// event folds into an Ask row.
#[test]
fn without_a_twin_the_next_connection_is_folded_and_counted_once() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![ask("ask-1", 1_000, Some("allow"))]),
            insert(vec![answer_event("event-2", 5_000)]),
            insert(vec![answer_event("event-3", 6_000)]),
        ],
    );
    assert_eq!(ids, ["ask-1", "event-3"]);
    assert_eq!(c.repeat_count("ask-1"), 2);
    assert_eq!(c.repeat_count("event-3"), 1);
}

#[test]
fn a_twin_never_folds_across_verdicts_rules_or_flows() {
    let deny = ConnectionRow {
        action: Some("deny".into()),
        matched_rule: Some("snitchwatch-deny-example.com-443-ppython3".into()),
        ..answer_event("event-2", 2_000)
    };
    let other_rule = ConnectionRow {
        matched_rule: Some("snitchwatch-allow-other-443".into()),
        ..answer_event("event-3", 2_000)
    };
    let other_port = ConnectionRow {
        dst_port: 80,
        ..answer_event("event-4", 2_000)
    };
    let not_answer = ConnectionRow {
        matched_rule: Some("allow-python".into()),
        ..answer_event("event-5", 2_000)
    };
    for twin in [deny, other_rule, other_port, not_answer] {
        let mut c = RowCollapse::default();
        let mut ask_row = ask("ask-1", 1_000, Some("allow"));
        if twin.matched_rule.as_deref() == Some("allow-python") {
            ask_row.matched_rule = twin.matched_rule.clone();
        }
        let ids = shown(
            &mut c,
            vec![insert(vec![ask_row]), insert(vec![twin.clone()])],
        );
        assert_eq!(ids.len(), 2, "{} folded", twin.id);
        assert_eq!(c.repeat_count("ask-1"), 1);
    }
}

#[test]
fn a_twin_needs_a_decided_ask_and_the_answer_window() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![ask("ask-1", 1_000, None)]),
            insert(vec![answer_event("event-2", 1_500)]),
        ],
    );
    assert_eq!(
        ids,
        ["ask-1", "event-2"],
        "a pending Ask never takes a twin"
    );

    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![ask("ask-1", 1_000, Some("allow"))]),
            insert(vec![answer_event("event-2", 1_000 + TWIN_WINDOW_MS + 1)]),
            insert(vec![answer_event("event-3", 999)]),
        ],
    );
    assert_eq!(ids, ["ask-1", "event-2", "event-3"], "outside the window");
}

#[test]
fn a_folded_twin_is_released_when_the_ask_row_changes() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![ask("ask-1", 1_000, Some("allow"))]),
            insert(vec![answer_event("event-2", 2_000)]),
            ServerMessage::UpdateConnectionRows {
                rows: vec![ConnectionRow {
                    action: Some("deny".into()),
                    ..ask("ask-1", 1_000, Some("allow"))
                }],
            },
        ],
    );
    assert_eq!(ids, ["ask-1", "event-2"]);
    assert_eq!(c.repeat_count("ask-1"), 1);
}

/// PR #167 review: after "From now" a new connection must not fold into
/// an Ask row from before the cut (the filter hides that row).
#[test]
fn sealing_stops_an_ask_row_taking_a_twin() {
    let mut c = RowCollapse::default();
    let mut store = super::super::row_store::RowStore::new();
    for out in c.rewrite(insert(vec![ask("ask-1", 1_000, Some("allow"))])) {
        store.apply(out);
    }
    c.seal();
    for out in c.rewrite(insert(vec![answer_event("event-2", 2_000)])) {
        store.apply(out);
    }
    let ids: Vec<&str> = store.rows().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["ask-1", "event-2"]);
    assert_eq!(c.repeat_count("ask-1"), 1);
}

#[test]
fn a_deferred_ask_row_never_takes_a_twin() {
    let mut c = RowCollapse::default();
    let deferred = ConnectionRow {
        deferred: true,
        ..ask("ask-1", 1_000, Some("allow"))
    };
    let ids = shown(
        &mut c,
        vec![
            insert(vec![deferred]),
            insert(vec![answer_event("event-2", 2_000)]),
        ],
    );
    assert_eq!(ids, ["ask-1", "event-2"]);
}

/// PR #167 review: one Remove naming a representative and its repeat
/// removes both, in either order; nothing comes back.
#[test]
fn removing_a_representative_and_its_repeat_together_leaves_nothing() {
    for ids in [["event-1", "event-2"], ["event-2", "event-1"]] {
        let mut c = RowCollapse::default();
        let mut store = super::super::row_store::RowStore::new();
        let mut tree = super::super::grouping::GroupTree::default();
        for msg in [
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::RemoveConnectionRows {
                ids: ids.iter().map(|s| s.to_string()).collect(),
            },
        ] {
            for out in c.rewrite(msg) {
                match &out {
                    ServerMessage::InsertConnectionRows { rows }
                    | ServerMessage::UpdateConnectionRows { rows } => {
                        rows.iter().for_each(|r| tree.upsert_row(r))
                    }
                    ServerMessage::RemoveConnectionRows { ids } => {
                        ids.iter().for_each(|id| tree.remove_row(id))
                    }
                    _ => {}
                }
                store.apply(out);
            }
        }
        assert!(store.is_empty(), "{ids:?}: {:?}", store.rows());
        assert_eq!(tree.process_count(), 0, "{ids:?}: the tree kept a row");
        assert_eq!(c.state_size(), 0, "{ids:?}");
    }
}

/// PR #167 review: a flood of distinct flows over time keeps the
/// collapse state bounded by the flows of the last few windows.
#[test]
fn a_flood_of_distinct_flows_keeps_the_state_bounded() {
    let mut c = RowCollapse::default();
    flood_stays_bounded(&mut c, 0);
}

/// PR #167 review: one far-future start (clock skew) must not stop
/// expiry until real time catches up.
#[test]
fn a_far_future_row_does_not_stop_expiry() {
    let mut c = RowCollapse::default();
    let far = ConnectionRow {
        dst_port: 1,
        dst_host: "future.example".into(),
        ..event("event-future", i64::MAX / 2, "deny")
    };
    c.rewrite(insert(vec![far]));
    flood_stays_bounded(&mut c, 1);
}

/// Feed 600 s of 50 distinct flows and 5 never-twinned Asks a second,
/// then check the state against the last windows' flows (+ `extra`).
fn flood_stays_bounded(c: &mut RowCollapse, extra: usize) {
    let per_second = 50;
    let seconds = 600;
    let asks_per_second = 5;
    for s in 0..seconds {
        let rows = (0..per_second)
            .map(|i| ConnectionRow {
                dst_port: (s * per_second + i) as u16,
                ..event(&format!("event-{s}-{i}"), s * 1_000, "deny")
            })
            .collect();
        c.rewrite(insert(rows));
        // Answered Asks that never get a twin wait only so long.
        let asks = (0..asks_per_second)
            .map(|i| ConnectionRow {
                dst_port: (s * asks_per_second + i) as u16,
                ..ask(&format!("ask-{s}-{i}"), s * 1_000, Some("allow"))
            })
            .collect();
        c.rewrite(insert(asks));
    }
    // Open row + representative per flow of the last two windows, and
    // the Asks of twice the twin window plus one expiry period.
    let bound = (per_second * 2 * (2 * COLLAPSE_WINDOW_MS / 1_000 + 1)
        + asks_per_second * ((2 * TWIN_WINDOW_MS + COLLAPSE_WINDOW_MS) / 1_000 + 1))
        as usize
        + extra;
    assert!(
        c.state_size() <= bound,
        "state {} > bound {bound}",
        c.state_size()
    );
}

#[test]
fn an_ask_row_never_absorbs_a_matching_event() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![event("ask-1", 1_000, "deny")]),
            insert(vec![event("event-2", 1_100, "deny")]),
        ],
    );
    assert_eq!(ids, ["ask-1", "event-2"]);
}

#[test]
fn qualified_event_rows_collapse_too() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![insert(vec![
            event("4:event-1", 1_000, "deny"),
            event("4:event-2", 1_100, "deny"),
        ])],
    );
    assert_eq!(ids, ["4:event-1"]);
    assert_eq!(c.repeat_count("4:event-1"), 2);
}

#[test]
fn one_batch_holding_a_row_and_its_repeats_inserts_one_row() {
    let mut c = RowCollapse::default();
    let out = c.rewrite(insert(vec![
        event("event-1", 1_000, "deny"),
        event("event-2", 1_100, "deny"),
        event("ask-3", 1_200, "deny"),
        event("event-4", 1_300, "deny"),
    ]));
    assert_eq!(
        out,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("ask-3", 1_200, "deny")
            ]),
            ServerMessage::UpdateConnectionRows {
                rows: vec![event("event-1", 1_000, "deny")]
            },
        ]
    );
    assert_eq!(c.repeat_count("event-1"), 3);
}

#[test]
fn removing_an_absorbed_row_lowers_the_count_only() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::RemoveConnectionRows {
                ids: vec!["event-2".into()],
            },
        ],
    );
    assert_eq!(ids, ["event-1"]);
    assert_eq!(c.repeat_count("event-1"), 1);
}

#[test]
fn removing_a_representative_promotes_its_oldest_repeat() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
                event("event-3", 1_200, "deny"),
            ]),
            ServerMessage::RemoveConnectionRows {
                ids: vec!["event-1".into()],
            },
            insert(vec![event("event-4", 1_300, "deny")]),
        ],
    );
    assert_eq!(ids, ["event-2"]);
    assert_eq!(c.repeat_count("event-2"), 3);
    assert_eq!(c.last_seen_ms("event-2"), Some(1_300));
    assert_eq!(c.repeat_count("event-1"), 1);
}

#[test]
fn a_move_of_an_absorbed_row_moves_its_representative() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![event("ask-0", 500, "allow")]),
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::MoveConnetionRows {
                ids: vec!["event-2".into()],
            },
        ],
    );
    assert_eq!(ids, ["event-1", "ask-0"]);
}

#[test]
fn an_update_that_changes_an_absorbed_row_releases_it() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::UpdateConnectionRows {
                rows: vec![event("event-2", 1_100, "allow")],
            },
        ],
    );
    assert_eq!(ids, ["event-1", "event-2"]);
    assert_eq!(c.repeat_count("event-1"), 1);
}

#[test]
fn an_update_that_keeps_the_key_stays_absorbed() {
    let mut c = RowCollapse::default();
    let mut later = event("event-2", 1_100, "deny");
    later.bytes_sent = 99;
    let ids = shown(
        &mut c,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::UpdateConnectionRows { rows: vec![later] },
        ],
    );
    assert_eq!(ids, ["event-1"]);
    assert_eq!(c.repeat_count("event-1"), 2);
}

#[test]
fn an_update_that_changes_a_representative_releases_its_repeats() {
    let mut c = RowCollapse::default();
    let mut store = super::super::row_store::RowStore::new();
    for msg in [
        insert(vec![
            event("event-1", 1_000, "deny"),
            event("event-2", 1_100, "deny"),
        ]),
        ServerMessage::UpdateConnectionRows {
            rows: vec![event("event-1", 1_000, "allow")],
        },
        insert(vec![event("event-3", 1_200, "deny")]),
    ] {
        for out in c.rewrite(msg) {
            store.apply(out);
        }
    }
    let shown: Vec<(&str, Option<&str>)> = store
        .rows()
        .iter()
        .map(|r| (r.id.as_str(), r.action.as_deref()))
        .collect();
    assert_eq!(
        shown,
        [
            ("event-1", Some("allow")),
            ("event-2", Some("deny")),
            ("event-3", Some("deny"))
        ]
    );
    assert_eq!(c.repeat_count("event-1"), 1);
}

#[test]
fn a_reinsert_of_a_known_id_is_an_update() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            insert(vec![event("event-2", 1_100, "deny")]),
            insert(vec![event("event-1", 1_000, "deny")]),
        ],
    );
    assert_eq!(ids, ["event-1"]);
    assert_eq!(c.repeat_count("event-1"), 2);
}

#[test]
fn clear_forgets_everything() {
    let mut c = RowCollapse::default();
    let ids = shown(
        &mut c,
        vec![
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::ClearConnectionRows,
            insert(vec![event("event-3", 1_200, "deny")]),
        ],
    );
    assert_eq!(ids, ["event-3"]);
    assert_eq!(c.repeat_count("event-1"), 1);
    assert_eq!(c.repeat_count("event-3"), 1);
}

#[test]
fn sealing_starts_a_new_row_for_the_next_repeat() {
    let mut c = RowCollapse::default();
    let mut store = super::super::row_store::RowStore::new();
    for out in c.rewrite(insert(vec![
        event("event-1", 1_000, "deny"),
        event("event-2", 1_100, "deny"),
    ])) {
        store.apply(out);
    }
    c.seal();
    for out in c.rewrite(insert(vec![event("event-3", 1_200, "deny")])) {
        store.apply(out);
    }
    let ids: Vec<&str> = store.rows().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["event-1", "event-3"]);
    assert_eq!(c.repeat_count("event-1"), 2, "sealing keeps the count");
}

/// #172: window sums at the ends of the i64 range must saturate, not panic
/// (debug builds overflowed on `last + WINDOW` / `first - WINDOW`).
#[test]
fn window_arithmetic_at_the_i64_extremes_does_not_overflow() {
    for at in [i64::MAX, i64::MAX - 1, i64::MIN, i64::MIN + 1] {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("event-1", at, "deny")]),
                insert(vec![event("event-2", at, "deny")]),
                insert(vec![ask("ask-1", at, Some("allow"))]),
                insert(vec![event("event-3", at, "allow")]),
                insert(vec![event("event-4", i64::MAX, "deny")]),
                insert(vec![event("event-5", i64::MIN, "deny")]),
            ],
        );
        assert!(!ids.is_empty(), "at={at}");
    }
}
