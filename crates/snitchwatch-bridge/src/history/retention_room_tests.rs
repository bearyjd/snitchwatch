//! PR 8 repair: making room is amortised and bounded. Eviction goes down to a
//! low-water mark, so the per-process scan of tier 2 is not repeated every
//! batch; tier 2 runs only when some process could be over its share; a chunk
//! of one process's rows starts after the previous one instead of rescanning
//! the prefix; and the chunks per attempt are bounded but not by a count that
//! progress would not reach.

use super::caps::Snapshot;
use super::measure::estimate_bytes;
use super::record::{Action, EventRecord, SnapshotRecord};
use super::retention::{pick_sql, Victims};
use super::retention_support::*;
use super::writer::{Item, TxnKind, EVICTION_ROUNDS};

fn low_water(rig: &Rig) -> u64 {
    let b = rig.engine.budget();
    b.high_water_pages() - b.max_page_count / 20
}

fn evict_reports(rig: &Rig) -> usize {
    rig.reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == TxnKind::Evict)
        .count()
}

/// A flooder holding `times` of its share, the newest rows.
fn flooder_at(rig: &Rig, flooder: &str, times: u64) {
    let mut ts = 1_000_000;
    while bytes_of(rig, flooder) <= times * share_bytes(rig) {
        fill(
            rig,
            (0..64).map(|_| {
                ts += 1;
                event(ts, flooder, Action::Allow)
            }),
        );
    }
}

fn batch_of(ts: i64, process: &str) -> Vec<Item> {
    (0..32).map(|i| plain(small_for(ts + i, process))).collect()
}

// ---- the low-water mark -----------------------------------------------------------

#[test]
fn the_low_water_mark_is_a_twentieth_of_the_file_under_the_high_water_mark() {
    let rig = rig();
    let b = rig.engine.budget();
    assert_eq!(b.low_water_pages(), low_water(&rig));
    assert!(b.low_water_pages() < b.high_water_pages());
}

#[test]
fn pressure_evicts_down_to_the_low_water_mark_not_just_under_the_high_one() {
    let mut rig = rig();
    fill_rotating(&rig, 12, 3, 0);
    assert!(rig.engine.make_room().unwrap());
    assert!(
        used(&rig) <= low_water(&rig),
        "{} pages, low water {}",
        used(&rig),
        low_water(&rig)
    );
}

#[test]
fn the_per_process_scan_is_amortised_over_many_pressured_batches() {
    let mut rig = rig();
    let flooder = "/opt/flood";
    flooder_at(&rig, flooder, 2);
    fill_rotating(&rig, 12, 3, 0);
    let mut batches = 0u64;
    let mut ts = 5_000_000;
    // Run until eviction has had to start several times over.
    while evict_runs(&rig) < 4 && batches < 600 {
        rig.engine.write_batch(&batch_of(ts, flooder)).unwrap();
        ts += 100;
        batches += 1;
    }
    let scans = rig.engine.tier2_scans();
    assert!(
        evict_runs(&rig) >= 4,
        "pressure came back: {batches} batches"
    );
    assert!(scans >= 1, "the flooder is over its share, so tier 2 ran");
    assert!(
        batches >= 20 * scans,
        "{scans} scans for {batches} batches: one scan serves many batches"
    );
}

/// Separate eviction attempts: a run of `Evict` reports is one attempt, and a
/// batch insert between them ends it.
fn evict_runs(rig: &Rig) -> usize {
    let reports = rig.reports.lock().unwrap();
    let mut runs = 0;
    let mut in_run = false;
    for r in reports.iter() {
        match r.kind {
            TxnKind::Evict => {
                if !in_run {
                    runs += 1;
                }
                in_run = true;
            }
            TxnKind::Insert => in_run = false,
            _ => {}
        }
    }
    runs
}

// ---- the gate on cheap state ------------------------------------------------------

#[test]
fn no_scan_runs_while_no_process_can_be_over_its_share() {
    let mut rig = rig();
    fill_rotating(&rig, 12, 3, 0);
    let mut ts = 5_000_000;
    let mut batches = 0;
    while evict_runs(&rig) < 12 && batches < 2_500 {
        let process = format!("/usr/bin/p{}", batches % 12);
        rig.engine.write_batch(&batch_of(ts, &process)).unwrap();
        ts += 100;
        batches += 1;
    }
    assert!(
        evict_runs(&rig) >= 12,
        "pressure came back: {batches} batches"
    );
    assert!(
        rig.engine.tier2_scans() <= 1,
        "one scan to learn who holds what, then none: {}",
        rig.engine.tier2_scans()
    );
}

/// A snapshot that only `process` refers to, `len` bytes long.
fn fat_rule(e: EventRecord, n: u8, len: usize) -> Item {
    let snapshot = Snapshot::new(&format!("{{\"rule\":\"{}\"}}", "x".repeat(len))).unwrap();
    let mut e = e;
    e.rule_hash = Some(hash(n));
    Item {
        snapshot: Some(SnapshotRecord::for_test(hash(n), snapshot, e.ts_ms)),
        event: e,
    }
}

#[test]
fn a_process_pushed_over_its_share_only_by_its_snapshots_is_still_trimmed() {
    let mut rig = rig();
    let process = "/opt/heavy";
    // Pressure from others' rows first, so the engine has learnt who holds
    // what; then empty most of the file again to make room for the process.
    fill_rotating(&rig, 12, 3, 0);
    assert!(rig.engine.make_room().unwrap());
    while used(&rig) > 400 {
        rig.engine.evict_chunk().unwrap();
    }
    // The process: events up to 1,000 bytes under its share (the newest rows,
    // so tier 3 would never reach them first), then a snapshot that takes it
    // over.
    let limit = share_bytes(&rig) - 1_000;
    let (mut held, mut ts, mut items) = (0, 1_000_000, Vec::new());
    for make in [big_for as fn(i64, &str) -> EventRecord, small_for] {
        while held + estimate_bytes(&make(0, process)) <= limit {
            ts += 1;
            held += estimate_bytes(&make(ts, process));
            items.push(plain(make(ts, process)));
        }
    }
    put(&mut rig, items);
    put(
        &mut rig,
        vec![fat_rule(small_for(ts + 1, process), 9, 3_000)],
    );
    let events_only: u64 = rig
        .engine
        .event_bytes()
        .unwrap()
        .into_iter()
        .filter(|(p, _)| p == process)
        .map(|(_, b)| b)
        .sum();
    assert!(events_only < share_bytes(&rig), "events alone are under it");
    assert!(
        bytes_of(&rig, process) > share_bytes(&rig),
        "with its snapshot it is over"
    );
    // Pressure again, from others.
    fill_rotating(&rig, 12, 3, 0);
    let before = count_of(&rig, process);
    assert!(rig.engine.make_room().unwrap());
    assert!(
        count_of(&rig, process) < before,
        "tier 2 took the process's own oldest rows"
    );
}

/// Rows of `process` up to `bytes`, as items.
fn rows_up_to(process: &str, bytes: u64, first_ts: i64) -> Vec<Item> {
    let (mut held, mut ts, mut items) = (0, first_ts, Vec::new());
    while held + estimate_bytes(&big_for(0, process)) <= bytes {
        ts += 1;
        held += estimate_bytes(&big_for(ts, process));
        items.push(plain(big_for(ts, process)));
    }
    items
}

/// The engine has learnt who holds what, and the file is mostly empty again.
fn learnt_and_emptied(rig: &mut Rig) {
    fill_rotating(rig, 12, 3, 0);
    assert!(rig.engine.make_room().unwrap());
    while used(rig) > 400 {
        rig.engine.evict_chunk().unwrap();
    }
}

fn through_the_pinned_path(rig: &mut Rig, items: Vec<Item>) {
    let registry = super::pins::PinRegistry::new();
    for chunk in items.chunks(32) {
        let batch = super::durable::PinnedBatch {
            items: chunk.to_vec(),
            pins: vec![registry.accept().unwrap()],
        };
        rig.engine.commit_pinned(&batch, &registry).unwrap();
    }
}

fn grows_over_its_share_after_the_tally_was_learnt(push: fn(&mut Rig, Vec<Item>)) {
    let mut rig = rig();
    let process = "/opt/grower";
    learnt_and_emptied(&mut rig);
    let share = share_bytes(&rig);
    put(&mut rig, rows_up_to(process, share * 9 / 10, 1_000_000));
    push(&mut rig, rows_up_to(process, share / 5, 2_000_000));
    assert!(bytes_of(&rig, process) > share, "it is over its share now");
    fill_rotating(&rig, 12, 3, 0);
    let before = count_of(&rig, process);
    assert!(rig.engine.make_room().unwrap());
    assert!(
        count_of(&rig, process) < before,
        "the tally followed what this insert path wrote, so tier 2 ran"
    );
}

#[test]
fn rows_a_batch_insert_adds_are_in_the_tally_that_gates_tier_two() {
    grows_over_its_share_after_the_tally_was_learnt(put);
}

#[test]
fn rows_a_pinned_commit_adds_are_in_the_tally_that_gates_tier_two() {
    grows_over_its_share_after_the_tally_was_learnt(through_the_pinned_path);
}

fn big_for(ts: i64, process: &str) -> EventRecord {
    let mut e = big(ts);
    e.process_path = Some(super::caps::ProcessPath::new(process));
    e
}

// ---- progress, not a count of rounds ----------------------------------------------

#[test]
fn eviction_goes_on_while_it_makes_progress_and_stops_at_the_chunk_limit() {
    // Within the limit: more chunks than the old three, and room is made.
    let mut rig = rig();
    flooder_at(&rig, "/opt/flood", 2);
    fill_rotating(&rig, 12, 3, 0);
    let others = count(&rig, "events") - count_of(&rig, "/opt/flood");
    rig.reports.lock().unwrap().clear();
    assert!(rig.engine.make_room().unwrap());
    let chunks = evict_reports(&rig);
    assert!(
        chunks > 3,
        "progress carried on past three chunks: {chunks}"
    );
    assert!(chunks <= EVICTION_ROUNDS as usize, "{chunks}");
    assert_eq!(
        count(&rig, "events") - count_of(&rig, "/opt/flood"),
        others,
        "tier 2 alone went down to the low-water mark, in as many chunks as it took"
    );
}

// ---- the cursor ---------------------------------------------------------------------

#[test]
fn a_later_chunk_of_one_process_seeks_past_the_chunk_before_it_through_the_time_index() {
    let rig = rig();
    let sql = pick_sql(Victims::Process("/p"), true);
    let plan: String = rig
        .engine
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(rusqlite::params!["/p", 5, 7, 32], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        plan.contains("SEARCH") && plan.contains("events_ts") && plan.contains("ts_ms>?"),
        "{plan}"
    );
}

/// SQLite VM steps the pick query takes for a chunk of 32.
fn pick_steps(rig: &Rig, victims: Victims<'_>, args: &[rusqlite::types::Value]) -> (i32, usize) {
    let mut stmt = rig
        .engine
        .connection()
        .prepare(&pick_sql(victims, args.len() > 2))
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args), |_| Ok(()))
        .unwrap()
        .count();
    (stmt.get_status(rusqlite::StatementStatus::VmStep), rows)
}

#[test]
fn a_later_chunk_does_not_rescan_the_rows_the_first_one_skipped() {
    let mut rig = rig();
    // A long prefix of other processes' older rows, then the process's.
    fill(
        &rig,
        (1..=4000).map(|ts| small_for(ts, &format!("/usr/bin/p{}", ts % 7))),
    );
    fill(
        &rig,
        (10_000..10_200).map(|ts| small_for(ts, "/opt/target")),
    );
    let victims = Victims::Process("/opt/target");
    let (_, first) = rig
        .engine
        .evict_victims_after(victims, u64::MAX, None)
        .unwrap();
    let cursor = first.cursor.expect("the chunk says where it ended");
    let p = |v: &str| rusqlite::types::Value::from(v.to_string());
    let int = |n: i64| rusqlite::types::Value::from(n);
    let (plain_steps, plain_rows) = pick_steps(&rig, victims, &[p("/opt/target"), int(32)]);
    let (seek_steps, seek_rows) = pick_steps(
        &rig,
        victims,
        &[p("/opt/target"), int(cursor.0), int(cursor.1), int(32)],
    );
    assert_eq!(
        (plain_rows, seek_rows),
        (32, 32),
        "the same rows either way"
    );
    assert!(
        seek_steps * 3 < plain_steps,
        "without the cursor {plain_steps} steps, with it {seek_steps}"
    );
    // And the engine's second chunk takes the next rows.
    let (_, second) = rig
        .engine
        .evict_victims_after(victims, u64::MAX, first.cursor)
        .unwrap();
    assert_eq!(second.rows, 32);
    assert_eq!(count_of(&rig, "/opt/target"), 200 - 64);
}

// ---- the tally is exact or unknown, never silently short ------------------------------

#[test]
fn a_tally_that_outgrows_its_cap_is_unknown_so_the_next_pressure_scans() {
    use super::retention_room::Tally;
    let mut tally = Tally::default();
    assert_eq!(tally.largest(), None, "unknown until a scan");
    tally.learnt(&[]);
    assert_eq!(tally.largest(), Some(0));
    let items: Vec<Item> = (0..1_100)
        .map(|i| plain(small_for(i, &format!("/usr/bin/p{i}"))))
        .collect();
    tally.inserted(&items[..1_000]);
    assert!(tally.largest().is_some_and(|b| b > 0));
    tally.inserted(&items[1_000..]);
    assert_eq!(tally.largest(), None, "past the cap it is dropped whole");
}

#[test]
fn deleting_rows_lowers_the_tally_by_what_the_rows_measured() {
    use super::retention_room::Tally;
    let mut tally = Tally::default();
    tally.learnt(&[("/p".to_string(), 1_000)]);
    tally.removed(&[("/p".to_string(), 400), ("/unknown".to_string(), 9)]);
    assert_eq!(tally.largest(), Some(600));
}

#[test]
fn an_eviction_lowers_the_engines_own_tally() {
    let mut rig = rig();
    rig.engine.tally.learnt(&[]);
    put(
        &mut rig,
        (1..=64).map(|ts| plain(small_for(ts, "/opt/p"))).collect(),
    );
    let before = rig.engine.tally.largest().unwrap();
    assert!(before > 0, "inserts are counted");
    evict_process(&mut rig, "/opt/p").unwrap();
    assert!(rig.engine.tally.largest().unwrap() < before);
}
