use super::*;

fn row(id: &str, action: Option<&str>) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: format!("proc-{id}"),
        process_path: None,
        dst_host: format!("host-{id}.example"),
        dst_ip: "1.1.1.1".to_string(),
        dst_port: 443,
        protocol: "tcp".to_string(),
        direction: "outgoing".to_string(),
        action: action.map(|s| s.to_string()),
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

fn ids(store: &RowStore) -> Vec<String> {
    store.rows().iter().map(|r| r.id.clone()).collect()
}

#[test]
fn verdict_maps_action_strings() {
    assert_eq!(Verdict::from_action(None), Verdict::Pending);
    assert_eq!(Verdict::from_action(Some("allow")), Verdict::Allowed);
    assert_eq!(Verdict::from_action(Some("deny")), Verdict::Denied);
    assert_eq!(
        Verdict::from_action(Some("blocklist")),
        Verdict::Blocklisted
    );
    assert_eq!(Verdict::from_action(Some("weird")), Verdict::Other);
    assert_eq!(Verdict::Pending.as_token(), "pending");
}

#[test]
fn insert_appends_in_order_and_reports_range() {
    let mut s = RowStore::new();
    let ops = s.apply(ServerMessage::InsertConnectionRows {
        rows: vec![row("a", Some("allow")), row("b", None)],
    });
    assert_eq!(ops, vec![ModelOp::Insert { at: 0, count: 2 }]);
    assert_eq!(ids(&s), vec!["a", "b"]);

    let ops2 = s.apply(ServerMessage::InsertConnectionRows {
        rows: vec![row("c", Some("deny"))],
    });
    assert_eq!(ops2, vec![ModelOp::Insert { at: 2, count: 1 }]);
    assert_eq!(ids(&s), vec!["a", "b", "c"]);
}

#[test]
fn count_new_ids_matches_actual_append_delta() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None)]);
    // "b" and "c" are new; "a" already exists.
    let batch = vec![row("b", None), row("a", Some("allow")), row("c", None)];
    let predicted = s.count_new_ids(&batch);
    let before = s.len();
    s.insert_rows(batch);
    assert_eq!(predicted, s.len() - before);
    assert_eq!(predicted, 2);
}

#[test]
fn count_new_ids_dedups_duplicate_new_id_within_one_batch() {
    // Regression: a single insert batch carrying the same *new* id twice.
    // insert_rows appends the first occurrence and folds the second into an
    // in-place update, so the real store grows by 1 — not 2. count_new_ids
    // must report exactly that, or the model wrapper's beginInsertRows
    // bracket desyncs from the store and violates a QAbstractListModel
    // invariant.
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None)]);
    let batch = vec![
        row("dup", None),
        row("dup", Some("allow")), // same new id, second time
        row("a", None),            // already present -> update
        row("fresh", None),        // genuinely new
    ];
    let predicted = s.count_new_ids(&batch);
    let before = s.len();
    let ops = s.insert_rows(batch);
    let actual = s.len() - before;

    assert_eq!(
        predicted, actual,
        "count_new_ids must equal insert_rows's real store growth"
    );
    assert_eq!(predicted, 2, "only 'dup' and 'fresh' are genuinely new");
    // The emitted Insert op's count agrees too.
    let inserted: usize = ops
        .iter()
        .map(|op| match op {
            ModelOp::Insert { count, .. } => *count,
            _ => 0,
        })
        .sum();
    assert_eq!(inserted, actual);
    // The second "dup" won: its action is the update-in-place value.
    let dup_idx = s.rows().iter().position(|r| r.id == "dup").unwrap();
    assert_eq!(s.verdict_at(dup_idx), Some(Verdict::Allowed));
}

#[test]
fn count_new_ids_all_existing_is_zero() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None)]);
    assert_eq!(s.count_new_ids(&[row("a", None), row("b", None)]), 0);
}

#[test]
fn insert_of_existing_id_updates_in_place() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None)]);
    let ops = s.insert_rows(vec![row("a", Some("allow")), row("c", None)]);
    // "a" changed in place at index 0; "c" appended at index 2.
    assert_eq!(
        ops,
        vec![
            ModelOp::Changed { indices: vec![0] },
            ModelOp::Insert { at: 2, count: 1 },
        ]
    );
    assert_eq!(ids(&s), vec!["a", "b", "c"]);
    assert_eq!(s.verdict_at(0), Some(Verdict::Allowed));
}

#[test]
fn update_replaces_matched_rows_and_ignores_unknown() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None)]);
    let ops = s.apply(ServerMessage::UpdateConnectionRows {
        rows: vec![row("b", Some("deny")), row("zzz", Some("allow"))],
    });
    assert_eq!(ops, vec![ModelOp::Changed { indices: vec![1] }]);
    assert_eq!(s.verdict_at(1), Some(Verdict::Denied));
    assert_eq!(s.len(), 2);
}

#[test]
fn remove_single_row() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None), row("c", None)]);
    let ops = s.apply(ServerMessage::RemoveConnectionRows {
        ids: vec!["b".to_string()],
    });
    assert_eq!(ops, vec![ModelOp::Remove { at: 1, count: 1 }]);
    assert_eq!(ids(&s), vec!["a", "c"]);
}

#[test]
fn remove_contiguous_run_is_one_op() {
    let mut s = RowStore::new();
    s.insert_rows(vec![
        row("a", None),
        row("b", None),
        row("c", None),
        row("d", None),
    ]);
    let ops = s.remove_rows(&["b".to_string(), "c".to_string()]);
    assert_eq!(ops, vec![ModelOp::Remove { at: 1, count: 2 }]);
    assert_eq!(ids(&s), vec!["a", "d"]);
}

#[test]
fn remove_non_contiguous_emits_high_index_first() {
    let mut s = RowStore::new();
    s.insert_rows(vec![
        row("a", None),
        row("b", None),
        row("c", None),
        row("d", None),
    ]);
    // remove indices 0 and 2 -> ops for the higher run first so indices stay valid
    let ops = s.remove_rows(&["c".to_string(), "a".to_string()]);
    assert_eq!(
        ops,
        vec![
            ModelOp::Remove { at: 2, count: 1 },
            ModelOp::Remove { at: 0, count: 1 },
        ]
    );
    assert_eq!(ids(&s), vec!["b", "d"]);
}

#[test]
fn remove_unknown_id_is_noop() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None)]);
    let ops = s.remove_rows(&["nope".to_string()]);
    assert!(ops.is_empty());
    assert_eq!(ids(&s), vec!["a"]);
}

#[test]
fn move_rows_to_front_reorders() {
    let mut s = RowStore::new();
    s.insert_rows(vec![
        row("a", None),
        row("b", None),
        row("c", None),
        row("d", None),
    ]);
    let ops = s.apply(ServerMessage::MoveConnetionRows {
        ids: vec!["c".to_string(), "a".to_string()],
    });
    // c and a move to front in the given id order.
    assert_eq!(ids(&s), vec!["c", "a", "b", "d"]);
    // Removed indices 0 (a) and 2 (c) high-first, then inserted 2 at front.
    assert_eq!(
        ops,
        vec![
            ModelOp::Remove { at: 2, count: 1 },
            ModelOp::Remove { at: 0, count: 1 },
            ModelOp::Insert { at: 0, count: 2 },
        ]
    );
}

#[test]
fn move_rows_already_at_front_is_noop() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None), row("c", None)]);
    let ops = s.move_rows_to_front(&["a".to_string(), "b".to_string()]);
    assert!(ops.is_empty());
    assert_eq!(ids(&s), vec!["a", "b", "c"]);
}

#[test]
fn remove_range_drains_and_clamps() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None), row("c", None)]);
    s.remove_range(1, 2);
    assert_eq!(ids(&s), vec!["a", "c"]);
    // Out-of-range end is clamped, not a panic.
    s.remove_range(1, 99);
    assert_eq!(ids(&s), vec!["a"]);
}

#[test]
fn clear_resets_and_empty_clear_is_noop() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", None)]);
    let ops = s.apply(ServerMessage::ClearConnectionRows);
    assert_eq!(ops, vec![ModelOp::Reset]);
    assert!(s.is_empty());

    let ops2 = s.clear();
    assert!(ops2.is_empty());
}

#[test]
fn visible_is_identity_when_unfiltered() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", Some("allow"))]);
    assert!(!s.is_filtered());
    assert_eq!(s.visible_len(), 2);
    assert_eq!(s.visible_row(0).unwrap().id, "a");
    assert_eq!(s.visible_row(1).unwrap().id, "b");
}

#[test]
fn pending_only_filter_projects_visible_subset() {
    use crate::connections::filter::ConnectionFilter;
    let mut s = RowStore::new();
    s.insert_rows(vec![
        row("a", None),
        row("b", Some("allow")),
        row("c", None),
    ]);
    let changed = s.set_filter(ConnectionFilter::new("", true));
    assert!(changed);
    assert!(s.is_filtered());
    assert_eq!(s.visible_len(), 2);
    assert_eq!(s.visible_row(0).unwrap().id, "a");
    assert_eq!(s.visible_row(1).unwrap().id, "c");
    assert_eq!(s.visible_verdict_at(0), Some(Verdict::Pending));
}

#[test]
fn setting_identical_filter_reports_no_change() {
    use crate::connections::filter::ConnectionFilter;
    let mut s = RowStore::new();
    assert!(s.set_filter(ConnectionFilter::new("x", false)));
    assert!(!s.set_filter(ConnectionFilter::new("x", false)));
}

#[test]
fn visible_projection_refreshes_after_mutation_when_filtered() {
    use crate::connections::filter::ConnectionFilter;
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None)]);
    s.set_filter(ConnectionFilter::new("", true));
    assert_eq!(s.visible_len(), 1);
    // A decided row arrives; caller recomputes the projection after applying.
    s.insert_rows(vec![row("b", Some("allow"))]);
    s.recompute_visible();
    assert_eq!(s.visible_len(), 1, "decided row excluded by pending_only");
    // Then a new pending row arrives.
    s.insert_rows(vec![row("c", None)]);
    s.recompute_visible();
    assert_eq!(s.visible_len(), 2);
}

#[test]
fn oldest_pending_started_at_ms_is_none_when_nothing_pending() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", Some("allow"))]);
    assert_eq!(s.oldest_pending_started_at_ms(10_000), None);
}

#[test]
fn oldest_pending_started_at_ms_picks_the_earliest_pending_row() {
    let mut s = RowStore::new();
    let mut newer = row("a", None);
    newer.started_at_ms = 2_000;
    let mut older = row("b", None);
    older.started_at_ms = 1_000;
    let decided = row("c", Some("deny")); // started_at_ms: 0, must be ignored
    s.insert_rows(vec![newer, older, decided]);
    assert_eq!(s.oldest_pending_started_at_ms(10_000), Some(1_000));
}

/// Codex review finding (P1): a naive "just take the oldest pending row"
/// picks up a permanently-stuck row whose AskRule already timed out
/// upstream at opensnitchd's own 120s deadline — a bridge older than #39
/// has no reaper for a cancelled verdict oneshot, so that row can stay
/// "pending" forever. Its age then exceeds the caller's ceiling check
/// too, hiding the pending-decision-exposure warning entirely for a
/// second, genuinely active row that arrives afterward. This must
/// select the fresh row, not the stuck one.
#[test]
fn oldest_pending_started_at_ms_ignores_rows_past_the_ask_rule_timeout() {
    let mut s = RowStore::new();
    let now = 10 * RowStore::ASK_RULE_TIMEOUT_MS;
    let mut expired = row("stuck", None);
    expired.started_at_ms = now - RowStore::ASK_RULE_TIMEOUT_MS - 1; // just past 120s
    let mut fresh = row("active", None);
    fresh.started_at_ms = now - 5_000; // 5s old, genuinely active
    s.insert_rows(vec![expired, fresh]);
    assert_eq!(
        s.oldest_pending_started_at_ms(now),
        Some(now - 5_000),
        "must pick the fresh active row, not the permanently-stuck expired one"
    );
}

#[test]
fn oldest_pending_started_at_ms_is_none_when_all_pending_rows_are_expired() {
    let mut s = RowStore::new();
    let now = 10 * RowStore::ASK_RULE_TIMEOUT_MS;
    let mut expired = row("stuck", None);
    expired.started_at_ms = now - RowStore::ASK_RULE_TIMEOUT_MS - 1;
    s.insert_rows(vec![expired]);
    assert_eq!(s.oldest_pending_started_at_ms(now), None);
}

#[test]
fn is_pending_and_index_of_reflect_store() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None), row("b", Some("deny"))]);
    assert_eq!(s.index_of("b"), Some(1));
    assert_eq!(s.is_pending("a"), Some(true));
    assert_eq!(s.is_pending("b"), Some(false));
    assert_eq!(s.is_pending("nope"), None);
}

#[test]
fn unrelated_server_message_is_ignored() {
    let mut s = RowStore::new();
    s.insert_rows(vec![row("a", None)]);
    let ops = s.apply(ServerMessage::SetRules { rules: vec![] });
    assert!(ops.is_empty());
    assert_eq!(s.len(), 1);
}

#[test]
fn matched_rule_display_pending_row_shows_awaiting_decision() {
    let mut r = row("p1", None);
    r.matched_rule = None;
    assert_eq!(matched_rule_display(&r), "— awaiting decision");
}

#[test]
fn matched_rule_display_decided_row_shows_the_rule_name() {
    let mut r = row("a", Some("allow"));
    r.matched_rule = Some("899-firefox-allow-out.json".to_string());
    assert_eq!(matched_rule_display(&r), "899-firefox-allow-out.json");
}

#[test]
fn matched_rule_display_decided_row_without_rule_name_shows_default_action() {
    let mut r = row("a", Some("allow"));
    r.matched_rule = None;
    assert_eq!(matched_rule_display(&r), "default action");
}

/// The rule "Show rule" and the 5-minute-block note read: none for a row
/// decided by default, even with a stray name (PR #108 review).
#[test]
fn matched_rule_name_is_empty_for_a_row_decided_by_default() {
    let mut r = row("a", Some("deny"));
    r.matched_rule = Some("899-curl-deny".to_string());
    assert_eq!(matched_rule_name(&r), "899-curl-deny");
    r.decided_by_default = true;
    assert_eq!(matched_rule_name(&r), "");
    r.matched_rule = None;
    r.decided_by_default = false;
    assert_eq!(matched_rule_name(&r), "");
}

/// E3: the daemon said no rule matched. The flag wins over any name, and
/// the display says which action the default applied.
#[test]
fn matched_rule_display_decided_by_default_names_the_default_action() {
    let mut r = row("a", Some("deny"));
    r.decided_by_default = true;
    assert_eq!(
        matched_rule_display(&r),
        "No rule: the firewall's default action (deny)"
    );
    r.action = Some("allow".to_string());
    assert_eq!(
        matched_rule_display(&r),
        "No rule: the firewall's default action (allow)"
    );
    // Never a rule's name, even a stray one.
    r.matched_rule = Some("stray".to_string());
    assert_eq!(
        matched_rule_display(&r),
        "No rule: the firewall's default action (allow)"
    );
}

#[test]
fn end_to_end_pending_then_decided_sequence() {
    // Mirrors the core loop: a pending row arrives, then is decided.
    let mut s = RowStore::new();
    s.apply(ServerMessage::InsertConnectionRows {
        rows: vec![row("p1", None)],
    });
    assert_eq!(s.verdict_at(0), Some(Verdict::Pending));

    s.apply(ServerMessage::UpdateConnectionRows {
        rows: vec![row("p1", Some("allow"))],
    });
    assert_eq!(s.verdict_at(0), Some(Verdict::Allowed));
    assert_eq!(s.len(), 1);
}
