use std::sync::Arc;

use super::*;
use crate::connections::attention::AttentionFacts;
use crate::rules::row_store::Rule;

const RULE: &str = "snitchwatch-default-chronyd";

fn row(id: &str, handled: bool) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: "chronyd".to_string(),
        process_path: Some("/usr/sbin/chronyd".to_string()),
        dst_host: "pool.ntp.org".to_string(),
        dst_ip: "10.0.0.1".to_string(),
        dst_port: 123,
        protocol: "udp".to_string(),
        direction: "outgoing".to_string(),
        action: Some(if handled { "allow" } else { "deny" }.to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        matched_rule: Some(RULE.to_string()),
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

fn collapsing_store() -> RowStore {
    let mut filter = ConnectionFilter::default();
    filter.set_needs_attention(true);
    let mut store = RowStore::with_filter(filter);
    let rule = Rule {
        name: RULE.to_string(),
        enabled: true,
        toggleable: Some(true),
        ..Rule::default()
    };
    store.set_attention_facts(Arc::new(AttentionFacts::new(
        &[rule],
        ["/usr/sbin/chronyd"],
    )));
    store
}

fn visible_ids(store: &RowStore) -> Vec<String> {
    (0..store.visible_len())
        .map(|i| store.visible_row(i).unwrap().id.clone())
        .collect()
}

/// Every step must leave `visible` exactly as a full recompute would.
fn assert_matches_recompute(store: &mut RowStore) {
    let incremental = store.visible.clone();
    store.recompute_visible();
    assert_eq!(incremental, store.visible, "incremental projection drifted");
}

#[test]
fn only_a_collapse_only_filter_is_incremental() {
    let mut store = collapsing_store();
    assert!(store.is_incremental());
    let mut searched = store.filter().clone();
    searched.set_query("ntp");
    store.set_filter(searched);
    assert!(!store.is_incremental(), "a search keeps the full path");
    let mut plain = collapsing_store().filter().clone();
    plain.set_needs_attention(false);
    assert!(!RowStore::with_filter(plain).is_incremental());
}

#[test]
fn steps_report_where_a_row_enters_or_leaves_the_visible_list() {
    let mut store = collapsing_store();
    let step = store.plan_upsert(&row("a", false));
    assert_eq!(step.change, VisibleChange::Inserted(0));
    store.commit_upsert(step, row("a", false));
    let step = store.plan_upsert(&row("h", true));
    assert_eq!(step.change, VisibleChange::None, "a handled row stays out");
    store.commit_upsert(step, row("h", true));
    let step = store.plan_upsert(&row("b", false));
    assert_eq!(step.change, VisibleChange::Inserted(1));
    store.commit_upsert(step, row("b", false));
    assert_eq!(visible_ids(&store), ["a", "b"]);

    // "h" becomes an exception: it enters between a and b.
    let step = store.plan_upsert(&row("h", false));
    assert_eq!(step.change, VisibleChange::Inserted(1));
    store.commit_upsert(step, row("h", false));
    assert_eq!(visible_ids(&store), ["a", "h", "b"]);
    // ... changes in place, then is handled again and leaves.
    let step = store.plan_upsert(&row("h", false));
    assert_eq!(step.change, VisibleChange::Changed(1));
    store.commit_upsert(step, row("h", false));
    let step = store.plan_upsert(&row("h", true));
    assert_eq!(step.change, VisibleChange::Removed(1));
    store.commit_upsert(step, row("h", true));
    assert_eq!(visible_ids(&store), ["a", "b"]);
    assert_matches_recompute(&mut store);

    // Removing a hidden row shifts the later indices; a visible one leaves.
    let step = store.plan_remove("h").unwrap();
    assert_eq!(step.change, VisibleChange::None);
    store.commit_remove(step);
    assert_matches_recompute(&mut store);
    let step = store.plan_remove("a").unwrap();
    assert_eq!(step.change, VisibleChange::Removed(0));
    store.commit_remove(step);
    assert_eq!(visible_ids(&store), ["b"]);
    assert_matches_recompute(&mut store);
    assert!(store.plan_remove("gone").is_none());
}

#[test]
fn a_single_row_delta_does_no_full_recompute() {
    let mut store = collapsing_store();
    store.apply_incremental(ServerMessage::InsertConnectionRows {
        rows: (0..50).map(|i| row(&format!("r{i}"), i % 2 == 0)).collect(),
    });
    let before = store.full_recompute_count();
    store.apply_incremental(ServerMessage::InsertConnectionRows {
        rows: vec![row("new", false), row("new", true)],
    });
    store.apply_incremental(ServerMessage::UpdateConnectionRows {
        rows: vec![row("r1", true), row("r2", false), row("unknown", false)],
    });
    store.apply_incremental(ServerMessage::RemoveConnectionRows {
        ids: vec!["r3".to_string(), "r4".to_string()],
    });
    assert_eq!(
        store.full_recompute_count(),
        before,
        "a delta re-filtered the store"
    );
    assert!(
        store.row_by_id("unknown").is_none(),
        "an update never inserts"
    );
    assert_matches_recompute(&mut store);
}

#[test]
fn the_incremental_path_keeps_up_with_68k_rows() {
    let mut store = collapsing_store();
    // Filled directly: `insert_rows` dedups each row against the store, so
    // building 68k rows through it is quadratic and not what is measured.
    store.rows = (0..68_000)
        .map(|i| row(&format!("r{i}"), i % 3 != 0))
        .collect();
    store.recompute_visible();
    let before = store.full_recompute_count();
    let started = std::time::Instant::now();
    for i in 0..200 {
        let id = format!("r{}", 67_999 - i);
        store.apply_incremental(ServerMessage::UpdateConnectionRows {
            rows: vec![row(&id, i % 2 == 0)],
        });
    }
    let elapsed = started.elapsed();
    assert_eq!(store.full_recompute_count(), before);
    // Generous for a debug build on a loaded machine; a full re-filter per
    // delta costs several times this.
    assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
    assert_matches_recompute(&mut store);
}
