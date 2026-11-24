//! PR 8: age and size retention, eviction order and chunking, snapshot
//! cleanup on every deletion path, the `evicted` marker, the pause, and the
//! accounting feedback to admission. Budgets are small (16 MiB) and the
//! databases live under the target directory.

use rusqlite::Connection;

use super::caps::{DstHost, ProcessPath, Protocol, RuleClass, RuleName};
use super::measure::{estimate_bytes, row_bytes_sql};
use super::record::{insert_event, Action, AnsweredBy, EventRecord};

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

// ---- the rig ---------------------------------------------------------------

use std::sync::{Arc, Mutex};

use super::budget::Budget;
use super::caps::{Snapshot, RULE_HASH};
use super::ledger::{read_gaps, Gap, GapCause};
use super::record::SnapshotRecord;
use super::test_support::{state_dir, tempdir};
use super::writer::{Engine, Item, TxnKind, TxnReport, WriteError};
use super::{enable, locate, open_store, Consent, Location, Mode};

struct Rig {
    _root: tempfile::TempDir,
    #[allow(dead_code)]
    location: Location,
    engine: Engine,
    reports: Arc<Mutex<Vec<TxnReport>>>,
}

fn rig() -> Rig {
    rig_with(16 * 1024 * 1024)
}

fn rig_with(total: u64) -> Rig {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let consent = Consent {
        text_id: "t".into(),
        accepted_ms: 1,
    };
    enable(&location, consent).unwrap();
    let store = open_store(&location).unwrap();
    let mut engine = Engine::new(store, &location, Budget::new(total).unwrap()).unwrap();
    engine
        .connection()
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = reports.clone();
    engine.set_observer(Box::new(move |r| sink.lock().unwrap().push(*r)));
    Rig {
        _root: root,
        location,
        engine,
        reports,
    }
}

fn hash(n: u8) -> [u8; RULE_HASH] {
    [n; RULE_HASH]
}

fn event(ts: i64, process: &str, action: Action) -> EventRecord {
    let mut e = EventRecord::new(ts, action, AnsweredBy::Rule);
    e.process_path = Some(ProcessPath::new(process));
    e
}

fn with_rule(mut e: EventRecord, n: u8) -> Item {
    e.rule_hash = Some(hash(n));
    Item {
        snapshot: Some(SnapshotRecord::for_test(
            hash(n),
            Snapshot::new(&format!("{{\"rule\":{n}}}")).unwrap(),
            e.ts_ms,
        )),
        event: e,
    }
}

fn plain(e: EventRecord) -> Item {
    Item {
        event: e,
        snapshot: None,
    }
}

/// Inserts `items` through the writer, `rows_per_txn` at a time.
fn put(rig: &mut Rig, items: Vec<Item>) {
    for chunk in items.chunks(rig.engine.budget().rows_per_txn as usize) {
        rig.engine.insert_batch(chunk).unwrap();
    }
}

fn count(rig: &Rig, table: &str) -> i64 {
    rig.engine
        .connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn remaining(rig: &Rig) -> Vec<(i64, i64)> {
    rig.engine
        .connection()
        .prepare("SELECT ts_ms, id FROM events ORDER BY ts_ms, id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn snapshots(rig: &Rig) -> Vec<Vec<u8>> {
    rig.engine
        .connection()
        .prepare("SELECT rule_hash FROM rules_seen ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn evicted(rig: &Rig) -> Vec<Gap> {
    read_gaps(rig.engine.connection())
        .unwrap()
        .into_iter()
        .filter(|g| g.cause == GapCause::Evicted)
        .collect()
}

/// Every removed event's instant lies inside some `evicted` row: the view
/// never shows a silent empty interval where events used to be.
fn assert_removed_are_covered(rig: &Rig, removed_ts: &[i64]) {
    let rows = evicted(rig);
    for ts in removed_ts {
        assert!(
            rows.iter().any(|g| g.start_ms <= *ts && *ts <= g.end_ms),
            "event at {ts} was removed without an evicted row covering it: {rows:?}"
        );
    }
}

fn fail_once() -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let mut armed = true;
    Box::new(move || {
        std::mem::take(&mut armed).then(|| WriteError::Io(std::io::Error::other("injected")))
    })
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
    rig.engine.evict_before_chunk(150).unwrap();
    assert_eq!(count(&rig, "events"), 2);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()], "rule 1 went, 2 is shared");
    rig.engine.evict_before_chunk(250).unwrap();
    assert_eq!(count(&rig, "events"), 1);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()], "still referenced by /c");
}

#[test]
fn deleting_one_event_by_process_share_removes_its_unreferenced_snapshot() {
    let mut rig = rig();
    snapshot_fixture(&mut rig);
    rig.engine.evict_process_chunk("/a").unwrap();
    assert_eq!(count(&rig, "events"), 2);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()]);
    rig.engine.evict_process_chunk("/b").unwrap();
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()], "/c still uses rule 2");
    rig.engine.evict_process_chunk("/c").unwrap();
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
            0 => rig.engine.evict_before_chunk(150),
            1 => rig.engine.evict_process_chunk("/a"),
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
    rig.engine.evict_process_chunk("/flood").unwrap();
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
