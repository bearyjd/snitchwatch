//! PR 8: age and size retention, eviction order and chunking, snapshot
//! cleanup on every deletion path, the `evicted` marker, the pause, and the
//! accounting feedback to admission. Budgets are small (16 MiB) and the
//! databases live under the target directory.

use rusqlite::Connection;

use super::caps::{DstHost, ProcessPath, Protocol, RuleClass, RuleName};
use super::measure::{estimate_bytes, row_bytes_sql};
use super::record::{insert_event, Action, AnsweredBy, EventRecord};
use super::retention_support::*;
use super::writer::{Item, TxnKind, WriteError};

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
    conn.prepare(&format!(
        "SELECT {} FROM events ORDER BY id",
        row_bytes_sql()
    ))
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
    put(
        &mut rig,
        (0..100)
            .map(|i| plain(event(i, "/p", Action::Allow)))
            .collect(),
    );
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
    assert_eq!(
        snapshots(&rig),
        vec![hash(2).to_vec()],
        "rule 1 went, 2 is shared"
    );
    evict_before(&mut rig, 250).unwrap();
    assert_eq!(count(&rig, "events"), 1);
    assert_eq!(
        snapshots(&rig),
        vec![hash(2).to_vec()],
        "still referenced by /c"
    );
}

#[test]
fn deleting_one_event_by_process_share_removes_its_unreferenced_snapshot() {
    let mut rig = rig();
    snapshot_fixture(&mut rig);
    evict_process(&mut rig, "/a").unwrap();
    assert_eq!(count(&rig, "events"), 2);
    assert_eq!(snapshots(&rig), vec![hash(2).to_vec()]);
    evict_process(&mut rig, "/b").unwrap();
    assert_eq!(
        snapshots(&rig),
        vec![hash(2).to_vec()],
        "/c still uses rule 2"
    );
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

/// A fault that fires on the `n`th commit attempt (1 is the first).
fn fail_on_commit(n: usize) -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let mut seen = 0;
    Box::new(move || {
        seen += 1;
        (seen == n).then(|| WriteError::Io(std::io::Error::other("injected")))
    })
}

#[test]
fn a_fault_on_any_commit_leaves_all_or_nothing_of_event_snapshot_and_marker_on_every_path() {
    for path in 0..3 {
        // A chunk is one transaction, so a fault on the second commit never
        // fires; were the marker its own transaction, it would, and the rows
        // would be gone without it.
        for commit in 1..=2 {
            let mut rig = rig();
            snapshot_fixture(&mut rig);
            rig.engine
                .fault_before_commit_for_test(fail_on_commit(commit));
            let outcome = match path {
                0 => evict_before(&mut rig, 150),
                1 => evict_process(&mut rig, "/a"),
                _ => rig.engine.evict_chunk(),
            };
            let what = format!("path {path}, fault on commit {commit}");
            let removed = 3 - count(&rig, "events");
            if outcome.is_err() {
                assert_eq!(removed, 0, "{what}: an event went with no commit");
                assert_eq!(snapshots(&rig).len(), 2, "{what}");
                assert!(evicted(&rig).is_empty(), "{what}: a marker with no delete");
            } else {
                assert!(removed > 0, "{what}");
                let marked: u64 = evicted(&rig).iter().map(|g| g.count).sum();
                assert_eq!(marked, removed as u64, "{what}: the delete has its marker");
            }
        }
    }
}

// ---- the evicted marker (D6 / L1) ----------------------------------------------

#[test]
fn the_marker_is_written_in_the_deleting_transaction_with_the_earliest_remaining_time() {
    let mut rig = rig();
    put(
        &mut rig,
        (1..=40)
            .map(|i| plain(event(i * 10, "/p", Action::Allow)))
            .collect(),
    );
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
    put(
        &mut rig,
        (1..=100)
            .map(|i| plain(event(i * 10, "/p", Action::Allow)))
            .collect(),
    );
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
    assert_eq!(
        (rows[0].start_ms, rows[0].end_ms, rows[0].count),
        (200, 400, 2)
    );
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
    put(
        rig,
        stamps
            .iter()
            .map(|ts| plain(event(*ts, "/p", Action::Allow)))
            .collect(),
    );
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
    expire_all(&mut rig, NOW);
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
    assert_eq!(
        aged(&mut rig, NOW).removed,
        100,
        "10% of 1001, rounded down"
    );
    assert_eq!(count(&rig, "events"), 901);
    // The budget holds an eighth: 125 less the 100 just spent.
    assert_eq!(aged(&mut rig, NOW).removed, 25);
    // A day of uptime refills it, and the pass is a tenth again.
    rig.engine.freeze_expiry_clock();
    rig.engine
        .advance_expiry_clock(std::time::Duration::from_secs(86_400));
    assert_eq!(aged(&mut rig, NOW).removed, 87, "a tenth of 876");
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
    put(
        &mut rig,
        (1..=30)
            .map(|i| plain(small_for(i, "/usr/bin/small")))
            .collect(),
    );
    let mut ts = 1_000;
    let mut evicted_any = false;
    for _ in 0..80 {
        assert!(rig.engine.make_room().unwrap(), "recording paused");
        let batch: Vec<Item> = (0..32)
            .map(|_| {
                ts += 1;
                plain(big(ts))
            })
            .collect();
        rig.engine.insert_batch(&batch).unwrap();
        evicted_any |= count_of(&rig, &hostile_path()) < i64::from(ts as i32 - 1_000);
    }
    assert!(evicted_any, "the flood must have overrun the budget");
    assert_eq!(
        count_of(&rig, "/usr/bin/small"),
        30,
        "others stay while the flooder is over share"
    );
    assert!(rig.engine.file_pages().unwrap() <= rig.engine.budget().max_page_count);
}

#[test]
fn a_process_under_its_share_is_never_a_tier_two_victim() {
    let mut rig = rig();
    fill_rotating(&rig, 12, 0, 0);
    let share = share_bytes(&rig);
    for (p, b) in rig.engine.process_bytes().unwrap() {
        assert!(
            b < share,
            "{p} holds {b} of a {share} share: the fixture needs all under"
        );
    }
    let before = remaining(&rig);
    assert!(!rig.engine.make_room().unwrap() || used(&rig) <= high_water(&rig));
    let after = remaining(&rig);
    let removed = before.len() - after.len();
    assert!(removed > 0, "pressure must have evicted something");
    assert_eq!(
        after,
        before[removed..].to_vec(),
        "global order: the oldest rows, whoever's"
    );
}

#[test]
fn an_over_share_process_is_trimmed_first_and_only_then_are_others_touched() {
    let mut rig = rig();
    let flooder = "/opt/flood";
    // The flooder is the newest, and just over its share (tiny rows: a chunk
    // of them frees little space, so tier three must run too).
    let mut ts = 1_000_000;
    while bytes_of(&rig, flooder) <= share_bytes(&rig) {
        fill(
            &rig,
            (0..64).map(|_| {
                ts += 1;
                event(ts, flooder, Action::Allow)
            }),
        );
    }
    fill_rotating(&rig, 12, 12, 0);
    let (flood_before, others_before) = (
        count_of(&rig, flooder),
        count(&rig, "events") - count_of(&rig, flooder),
    );
    let others_oldest: Vec<(i64, i64)> = remaining(&rig)
        .into_iter()
        .filter(|r| r.0 < 1_000_000)
        .take(5)
        .collect();
    rig.engine.make_room().unwrap();
    assert!(
        bytes_of(&rig, flooder) <= share_bytes(&rig),
        "tier two trimmed the flooder to its share"
    );
    assert!(count_of(&rig, flooder) < flood_before);
    let others_after = count(&rig, "events") - count_of(&rig, flooder);
    assert!(
        others_after < others_before,
        "tier three then took others' rows"
    );
    assert!(
        remaining(&rig)
            .iter()
            .all(|r| !others_oldest[..1].contains(r)),
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
    assert_eq!(
        bytes_of(&rig, "/a"),
        2 * row + snap(1),
        "rule 1 is only /a's; rule 2 is shared"
    );
    assert_eq!(
        bytes_of(&rig, "/b"),
        row,
        "/b gets no part of the shared snapshot"
    );
}

// ---- the size limit, the journal bound and S4 -----------------------------------

/// A worst-case-ish row (long host and rule name) from one of `n` processes.
fn heavy(ts: i64, n: i64) -> EventRecord {
    let mut e = event(ts, &format!("/usr/bin/p{}", ts % n), Action::Allow);
    e.dst_host = Some(DstHost::new(&"h".repeat(250)));
    e.rule_name = Some(RuleName::new(&"r".repeat(250)));
    e
}

#[test]
fn the_size_limit_takes_the_oldest_chunks_and_never_lets_the_file_or_journal_pass_the_budget() {
    let mut rig = rig();
    let budget = *rig.engine.budget();
    let mut ts = 0;
    let mut evictions = 0;
    for _ in 0..900 {
        assert!(
            rig.engine.make_room().unwrap(),
            "recording paused at row {ts}"
        );
        let batch: Vec<Item> = (0..32)
            .map(|_| {
                ts += 1;
                plain(heavy(ts, 12))
            })
            .collect();
        rig.engine.insert_batch(&batch).unwrap();
        evictions += usize::from(count(&rig, "events") < ts);
        assert!(rig.engine.file_pages().unwrap() <= budget.max_page_count);
    }
    assert!(
        evictions > 100,
        "the run must have been under pressure: {evictions}"
    );
    let reports = rig.reports.lock().unwrap().clone();
    for r in &reports {
        assert!(r.journal_bytes <= budget.journal_limit_bytes(), "{r:?}");
        if r.kind == TxnKind::Evict {
            assert!(r.units <= budget.rows_per_txn, "{r:?}");
        }
    }
    // Only the oldest went, in whole chunks: what is left is the newest run.
    let left: Vec<i64> = remaining(&rig).iter().map(|r| r.0).collect();
    let first = left[0];
    assert!(
        left.iter().copied().eq(first..=ts),
        "a contiguous newest run"
    );
    assert_removed_are_covered(&rig, &(1..first).collect::<Vec<_>>());
}

#[test]
fn deleting_ninety_percent_returns_the_space_without_a_journal_over_the_limit() {
    let mut rig = rig();
    let budget = *rig.engine.budget();
    let target = budget.max_page_count - budget.ledger_pages - 20;
    let mut ts = 0;
    while rig.engine.file_pages().unwrap() < target {
        fill(
            &rig,
            (0..32).map(|_| {
                ts += 1;
                heavy(ts, 12)
            }),
        );
    }
    let (before, rows) = (rig.engine.file_pages().unwrap(), count(&rig, "events"));
    rig.reports.lock().unwrap().clear();
    while count(&rig, "events") > rows / 10 {
        rig.engine.evict_chunk().unwrap();
    }
    rig.engine.vacuum_all().unwrap();
    let after = rig.engine.file_pages().unwrap();
    assert!(
        after < before / 5,
        "the file shrank from {before} to {after} pages"
    );
    for r in rig.reports.lock().unwrap().iter() {
        assert!(r.journal_bytes <= budget.journal_limit_bytes(), "{r:?}");
    }
}

// ---- eviction deletes nothing but rows of its own database -----------------------

#[test]
fn a_fill_to_the_limit_a_pause_and_an_age_sweep_touch_no_other_file() {
    let mut rig = rig();
    let dir = rig.location.db_path().parent().unwrap().to_path_buf();
    let canaries = [
        dir.join("history.db.bak"),
        dir.join("notes.txt"),
        dir.join("sub").join("inner.txt"),
    ];
    std::fs::create_dir(dir.join("sub")).unwrap();
    for (i, path) in canaries.iter().enumerate() {
        std::fs::write(path, format!("canary {i}")).unwrap();
    }
    let settings = std::fs::read(rig.location.settings_path()).unwrap();
    let mut ts = NOW - 40 * DAY_MS;
    for _ in 0..500 {
        assert!(rig.engine.make_room().unwrap());
        let batch: Vec<Item> = (0..32)
            .map(|_| {
                ts += 60_000;
                plain(heavy(ts, 12))
            })
            .collect();
        rig.engine.insert_batch(&batch).unwrap();
    }
    rig.engine.set_retention_days(7);
    expire_all(&mut rig, NOW);
    fill_until_paused(&mut rig);
    for (i, path) in canaries.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("canary {i}")
        );
    }
    assert_eq!(
        std::fs::read(rig.location.settings_path()).unwrap(),
        settings
    );
    assert!(rig.location.db_path().exists());
}

#[test]
fn expiry_returns_the_freed_space_to_the_filesystem() {
    let mut rig = rig();
    let old = NOW - 60 * DAY_MS;
    let mut ts = old;
    for _ in 0..40 {
        fill(
            &rig,
            (0..32).map(|_| {
                ts += 1;
                heavy(ts, 12)
            }),
        );
    }
    let before = rig.engine.file_pages().unwrap();
    expire_all(&mut rig, NOW);
    assert_eq!(count(&rig, "events"), 0);
    assert!(
        rig.engine.file_pages().unwrap() < before / 4,
        "S4: {} of {before}",
        rig.engine.file_pages().unwrap()
    );
}

#[test]
fn tier_two_stops_trimming_once_the_store_is_under_high_water() {
    let mut rig = rig();
    let flooder = "/opt/flood";
    let mut ts = 1_000_000;
    while bytes_of(&rig, flooder) <= 2 * share_bytes(&rig) {
        fill(
            &rig,
            (0..64).map(|_| {
                ts += 1;
                event(ts, flooder, Action::Allow)
            }),
        );
    }
    fill_rotating(&rig, 12, 3, 0);
    assert!(rig.engine.make_room().unwrap());
    assert!(used(&rig) <= high_water(&rig));
    assert!(
        bytes_of(&rig, flooder) > share_bytes(&rig),
        "it took only what the pressure needed, not all the way to the share"
    );
}
