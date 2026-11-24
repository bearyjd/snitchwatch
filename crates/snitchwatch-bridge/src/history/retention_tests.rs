//! PR 8: age and size retention, eviction order and chunking, snapshot
//! cleanup on every deletion path, the `evicted` marker, the pause, and the
//! accounting feedback to admission. Budgets are small (16 MiB) and the
//! databases live under the target directory.

use rusqlite::Connection;

use super::caps::{DstHost, ProcessPath, Protocol, RuleClass, RuleName};
use super::measure::{estimate_bytes, row_bytes_sql};
use super::record::{insert_event, Action, AnsweredBy, EventRecord};
use super::retention_support::*;
use super::writer::{Item, TxnKind};

fn full_record() -> EventRecord {
    let mut e = EventRecord::new(1, Action::Allow, AnsweredBy::Rule);
    e.process_path = Some(ProcessPath::new("/usr/bin/\u{e9}\u{4e16}"));
    e.dst_host = Some(DstHost::new("example.org"));
    e.protocol = Some(Protocol::new("tcp"));
    e.rule_name = Some(RuleName::new("r\u{1f600}"));
    e.rule_class = Some(RuleClass::new("exact"));
    e
}

fn sql_bytes(conn: &Connection) -> Vec<u64> {
    conn.prepare(&format!("SELECT {} FROM events ORDER BY id", row_bytes_sql()))
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .map(|v| v.unwrap() as u64)
        .collect()
}

// ---- the shared byte measure (decision 2c) ---------------------------------

#[test]
fn the_sql_row_measure_equals_the_admission_estimate_for_every_shape() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&super::schema::schema_sql()).unwrap();
    let bare = EventRecord::new(2, Action::Deny, AnsweredBy::Rule);
    let mut some = EventRecord::new(3, Action::Allow, AnsweredBy::Gui);
    some.process_path = Some(ProcessPath::new("/p"));
    let rows = [full_record(), bare, some];
    for r in &rows {
        insert_event(&conn, r).unwrap();
    }
    let rust: Vec<u64> = rows.iter().map(estimate_bytes).collect();
    assert_eq!(sql_bytes(&conn), rust);
    assert!(rust[0] > rust[1], "multi-byte text counts by bytes");
}

// ---- order and chunking (D5) ---------------------------------------------------

#[test]
fn global_eviction_is_strictly_by_time_then_id_whatever_the_verdict() {
    let mut rig = rig();
    let verdicts = [Action::Allow, Action::Deny, Action::PendingUnanswered];
    // Timestamps out of id order, verdicts mixed, ties on the time.
    let items: Vec<Item> = (0..100_i64)
        .map(|i| {
            let ts = 1000 + (i * 37) % 50;
            plain(event(ts, "/p", verdicts[i as usize % 3]))
        })
        .collect();
    put(&mut rig, items);
    let mut model = remaining(&rig);
    while !model.is_empty() {
        rig.engine.evict_chunk().unwrap();
        model.drain(..model.len().min(32));
        assert_eq!(remaining(&rig), model, "the oldest 32 by (ts, id) went");
    }
}

#[test]
fn one_eviction_transaction_deletes_at_most_rows_per_txn() {
    let mut rig = rig();
    put(&mut rig, (0..100).map(|i| plain(event(i, "/p", Action::Allow))).collect());
    let n = rig.engine.budget().rows_per_txn;
    rig.reports.lock().unwrap().clear();
    while count(&rig, "events") > 0 {
        let r = rig.engine.evict_chunk().unwrap();
        assert!(r.units <= n && r.units > 0, "{r:?}");
    }
    let evictions: Vec<_> = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == TxnKind::Evict)
        .map(|r| r.units)
        .collect();
    assert_eq!(evictions, vec![32, 32, 32, 4]);
}

// ---- snapshot cleanup on every deletion path (D7) ------------------------------

/// Two events with their own snapshots (rules 1 and 2), one more sharing
/// rule 2 and surviving every path below it.
fn snapshot_fixture(rig: &mut Rig) {
    put(
        rig,
        vec![
            with_rule(event(100, "/a", Action::Allow), 1),
            with_rule(event(200, "/b", Action::Allow), 2),
            plain({
                let mut e = event(5_000, "/c", Action::Allow);
                e.rule_hash = Some(hash(2));
                e
            }),
        ],
    );
    assert_eq!(snapshots(rig).len(), 2);
}

#[test]
fn deleting_one_event_by_age_removes_its_unreferenced_snapshot_and_keeps_shared_ones() {
    let mut rig = rig();
    snapshot_fixture(&mut rig);
    evict_before(&mut rig, 150).unwrap();
    assert_eq!(count(&rig, "events"), 2);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()], "rule 1 went, 2 is shared");
    evict_before(&mut rig, 250).unwrap();
    assert_eq!(count(&rig, "events"), 1);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()], "still referenced by /c");
}

#[test]
fn deleting_one_event_by_process_share_removes_its_unreferenced_snapshot() {
    let mut rig = rig();
    snapshot_fixture(&mut rig);
    evict_process(&mut rig, "/a").unwrap();
    assert_eq!(count(&rig, "events"), 2);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()]);
    evict_process(&mut rig, "/b").unwrap();
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()], "/c still uses rule 2");
    evict_process(&mut rig, "/c").unwrap();
    assert!(snapshots(&rig).is_empty());
}

#[test]
fn deleting_one_event_by_global_order_removes_its_unreferenced_snapshot() {
    let mut rig = rig();
    snapshot_fixture(&mut rig);
    rig.engine.evict_chunk().unwrap();
    assert_eq!(count(&rig, "events"), 0, "all three fit one chunk");
    assert!(snapshots(&rig).is_empty());
}

#[test]
fn a_fault_before_commit_leaves_the_event_the_snapshot_and_no_marker_on_every_path() {
    for path in 0..3 {
        let mut rig = rig();
        snapshot_fixture(&mut rig);
        rig.engine.fault_before_commit_for_test(fail_once());
        let outcome = match path {
            0 => evict_before(&mut rig, 150),
            1 => evict_process(&mut rig, "/a"),
            _ => rig.engine.evict_chunk(),
        };
        assert!(outcome.is_err(), "path {path}");
        assert_eq!(count(&rig, "events"), 3, "path {path}");
        assert_eq!(snapshots(&rig).len(), 2, "path {path}");
        assert!(evicted(&rig).is_empty(), "path {path}: no marker without the delete");
    }
}

// ---- the evicted marker (D6 / L1) ----------------------------------------------

#[test]
fn the_marker_is_written_in_the_deleting_transaction_with_the_earliest_remaining_time() {
    let mut rig = rig();
    put(&mut rig, (1..=40).map(|i| plain(event(i * 10, "/p", Action::Allow))).collect());
    let removed: Vec<i64> = remaining(&rig).iter().take(32).map(|r| r.0).collect();
    rig.engine.evict_chunk().unwrap();
    let rows = evicted(&rig);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].count, 32);
    assert_eq!(rows[0].end_ms, 330, "the oldest event that is still there");
    assert!(rows[0].is_uncertain());
    assert_removed_are_covered(&rig, &removed);
    // The marker never claims that anything left was removed.
    assert!(remaining(&rig).iter().all(|(ts, _)| *ts >= rows[0].end_ms));
}

#[test]
fn consecutive_markers_merge_and_still_cover_everything_removed() {
    let mut rig = rig();
    put(&mut rig, (1..=100).map(|i| plain(event(i * 10, "/p", Action::Allow))).collect());
    let all: Vec<i64> = remaining(&rig).iter().map(|r| r.0).collect();
    while count(&rig, "events") > 0 {
        rig.engine.evict_chunk().unwrap();
    }
    let rows = evicted(&rig);
    assert_eq!(rows.len(), 1, "L1: adjacent markers merge: {rows:?}");
    assert_eq!(rows[0].count, 100);
    assert_removed_are_covered(&rig, &all);
}

#[test]
fn a_process_eviction_marker_spans_only_what_it_removed() {
    let mut rig = rig();
    put(
        &mut rig,
        vec![
            plain(event(100, "/other", Action::Allow)),
            plain(event(200, "/flood", Action::Allow)),
            plain(event(300, "/other", Action::Allow)),
            plain(event(400, "/flood", Action::Allow)),
            plain(event(500, "/other", Action::Allow)),
        ],
    );
    evict_process(&mut rig, "/flood").unwrap();
    let rows = evicted(&rig);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].start_ms, rows[0].end_ms, rows[0].count), (200, 400, 2));
    assert_removed_are_covered(&rig, &[200, 400]);
    assert_eq!(remaining(&rig).len(), 3, "the others are untouched");
}

// ---- age retention (D5) ---------------------------------------------------------

const DAY_MS: i64 = 86_400_000;
const NOW: i64 = 400 * DAY_MS;

fn aged(rig: &mut Rig, now: i64) -> super::retention::Expiry {
    rig.engine.expire_aged(now, false).unwrap()
}

fn put_at(rig: &mut Rig, stamps: &[i64]) {
    put(rig, stamps.iter().map(|ts| plain(event(*ts, "/p", Action::Allow))).collect());
}

#[test]
fn the_age_limit_is_exact_for_each_retention_choice() {
    for days in [7_u16, 30, 90] {
        let mut rig = rig();
        rig.engine.set_retention_days(days);
        let cutoff = NOW - i64::from(days) * DAY_MS;
        put_at(&mut rig, &[1, cutoff - 1, cutoff, cutoff + 1, NOW]);
        let pass = aged(&mut rig, NOW);
        assert_eq!(pass.removed, 2, "{days} days");
        let left: Vec<i64> = remaining(&rig).iter().map(|r| r.0).collect();
        assert_eq!(left, vec![cutoff, cutoff + 1, NOW], "{days} days");
        assert_removed_are_covered(&rig, &[1, cutoff - 1]);
    }
}

#[test]
fn thirty_days_is_the_default() {
    let mut rig = rig();
    let cutoff = NOW - 30 * DAY_MS;
    put_at(&mut rig, &[cutoff - 1, cutoff]);
    assert_eq!(aged(&mut rig, NOW).removed, 1);
}

#[test]
fn an_expiry_pass_works_in_rows_per_txn_chunks_and_covers_what_it_removed() {
    let mut rig = rig();
    let stamps: Vec<i64> = (0..100).map(|i| 1000 + i).collect();
    put_at(&mut rig, &stamps);
    put_at(&mut rig, &[NOW]);
    // 101 rows: a pass may take 10%, floored at one chunk: 32.
    rig.reports.lock().unwrap().clear();
    assert_eq!(aged(&mut rig, NOW).removed, 32);
    let units: Vec<u64> = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == TxnKind::Evict)
        .map(|r| r.units)
        .collect();
    assert_eq!(units, vec![32]);
    // Passes go on until nothing old is left; every chunk is within the bound.
    rig.reports.lock().unwrap().clear();
    while aged(&mut rig, NOW).removed > 0 {}
    assert_eq!(remaining(&rig).len(), 1);
    assert!(rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.kind != TxnKind::Evict || r.units <= 32));
    assert_removed_are_covered(&rig, &stamps);
}

#[test]
fn one_pass_expires_at_most_a_tenth_of_the_rows() {
    let mut rig = rig();
    let mut stamps: Vec<i64> = (0..1000).map(|i| 1000 + i).collect();
    stamps.push(NOW);
    put_at(&mut rig, &stamps);
    assert_eq!(aged(&mut rig, NOW).removed, 100, "10% of 1001, rounded down");
    assert_eq!(count(&rig, "events"), 901);
    assert_eq!(aged(&mut rig, NOW).removed, 90);
}

#[test]
fn a_pass_is_skipped_while_the_wall_clock_has_jumped() {
    let mut rig = rig();
    put_at(&mut rig, &[1000, 2000, NOW]);
    let skipped = rig.engine.expire_aged(NOW, true).unwrap();
    assert!(skipped.skipped_for_jump && skipped.removed == 0);
    assert_eq!(count(&rig, "events"), 3);
    assert!(evicted(&rig).is_empty());
    assert_eq!(aged(&mut rig, NOW).removed, 2, "the next pass goes ahead");
}

#[test]
fn a_clock_that_stepped_back_expires_nothing_extra() {
    let mut rig = rig();
    let cutoff = NOW - 30 * DAY_MS;
    put_at(&mut rig, &[cutoff + 5, cutoff + 10, NOW]);
    // The clock reads two days earlier than it did: the cutoff moves back too.
    assert_eq!(aged(&mut rig, NOW - 2 * DAY_MS).removed, 0);
    assert_eq!(count(&rig, "events"), 3);
}

// ---- the per-process byte share (D5) ---------------------------------------------

#[test]
fn a_flood_of_large_records_cannot_evict_another_processs_small_ones() {
    let mut rig = rig();
    // The small records are the oldest of all: global order would take them
    // first.
    put(&mut rig, (1..=30).map(|i| plain(small_for(i, "/usr/bin/small"))).collect());
    let mut ts = 1_000;
    let mut evicted_any = false;
    for _ in 0..80 {
        assert!(rig.engine.make_room().unwrap(), "recording paused");
        let batch: Vec<Item> = (0..32).map(|_| { ts += 1; plain(big(ts)) }).collect();
        rig.engine.insert_batch(&batch).unwrap();
        evicted_any |= count_of(&rig, &hostile_path()) < i64::from(ts as i32 - 1_000);
    }
    assert!(evicted_any, "the flood must have overrun the budget");
    assert_eq!(count_of(&rig, "/usr/bin/small"), 30, "others stay while the flooder is over share");
    assert!(rig.engine.file_pages().unwrap() <= rig.engine.budget().max_page_count);
}

#[test]
fn a_process_under_its_share_is_never_a_tier_two_victim() {
    let mut rig = rig();
    fill_rotating(&rig, 12, 0, 0);
    let share = share_bytes(&rig);
    for (p, b) in rig.engine.process_bytes().unwrap() {
        assert!(b < share, "{p} holds {b} of a {share} share: the fixture needs all under");
    }
    let before = remaining(&rig);
    assert!(!rig.engine.make_room().unwrap() || used(&rig) <= high_water(&rig));
    let after = remaining(&rig);
    let removed = before.len() - after.len();
    assert!(removed > 0, "pressure must have evicted something");
    assert_eq!(after, before[removed..].to_vec(), "global order: the oldest rows, whoever's");
}

#[test]
fn an_over_share_process_is_trimmed_first_and_only_then_are_others_touched() {
    let mut rig = rig();
    let flooder = "/opt/flood";
    // The flooder is the newest, and just over its share (tiny rows: a chunk
    // of them frees little space, so tier three must run too).
    let mut ts = 1_000_000;
    while bytes_of(&rig, flooder) <= share_bytes(&rig) {
        fill(&rig, (0..64).map(|_| { ts += 1; event(ts, flooder, Action::Allow) }));
    }
    fill_rotating(&rig, 12, 12, 0);
    let (flood_before, others_before) = (count_of(&rig, flooder), count(&rig, "events") - count_of(&rig, flooder));
    let others_oldest: Vec<(i64, i64)> = remaining(&rig).into_iter().filter(|r| r.0 < 1_000_000).take(5).collect();
    rig.engine.make_room().unwrap();
    assert!(bytes_of(&rig, flooder) <= share_bytes(&rig), "tier two trimmed the flooder to its share");
    assert!(count_of(&rig, flooder) < flood_before);
    let others_after = count(&rig, "events") - count_of(&rig, flooder);
    assert!(others_after < others_before, "tier three then took others' rows");
    assert!(
        remaining(&rig).iter().all(|r| !others_oldest[..1].contains(r)),
        "and they were the oldest ones"
    );
}

#[test]
fn snapshot_bytes_count_toward_a_process_only_when_attributable_to_it_alone() {
    let mut rig = rig();
    put(
        &mut rig,
        vec![
            with_rule(event(1, "/a", Action::Allow), 1),
            with_rule(event(2, "/a", Action::Allow), 2),
            plain({
                let mut e = event(3, "/b", Action::Allow);
                e.rule_hash = Some(hash(2));
                e
            }),
        ],
    );
    let snap = |n: u8| format!("{{\"rule\":{n}}}").len() as u64;
    let row = row_bytes_of("/a");
    assert_eq!(bytes_of(&rig, "/a"), 2 * row + snap(1), "rule 1 is only /a's; rule 2 is shared");
    assert_eq!(bytes_of(&rig, "/b"), row, "/b gets no part of the shared snapshot");
}

