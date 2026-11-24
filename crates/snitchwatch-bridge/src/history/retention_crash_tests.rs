//! PR 8 across a real `kill -9`: eviction killed in the middle of a chunk,
//! eviction killed just after one, and a pause killed with a lost batch's pin
//! held. A child test process builds the state, then SIGKILLs itself (no
//! unwinding, no `Drop`); this process reopens the file. The child is this
//! same test binary, selected by an environment variable.

use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Arc;

use rusqlite::Connection;

use super::budget::Budget;
use super::caps::{Snapshot, RULE_HASH};
use super::durable::tests::{engine_at, manual_clock};
use super::durable::PinnedBatch;
use super::ledger::{read_gaps, Gap, GapCause, LedgerLimits};
use super::pins::PinRegistry;
use super::record::{insert_event, Action, AnsweredBy, EventRecord, SnapshotRecord};
use super::session::read_watermark;
use super::test_support::{state_dir, tempdir};
use super::writer::Item;
use super::{enable, locate, Consent, Location, Mode};

const SCENARIO: &str = "SW_HISTORY_RETENTION_CRASH_SCENARIO";
const STATE: &str = "SW_HISTORY_RETENTION_CRASH_STATE";
const BIG: i64 = 9_000;

fn kill_self() -> ! {
    // SAFETY: kill(2) on our own pid; no memory is touched.
    unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    unreachable!("SIGKILL is not catchable");
}

fn item(ts: i64, hash: u8) -> Item {
    let mut event = EventRecord::new(ts, Action::Allow, AnsweredBy::Rule);
    event.rule_hash = Some([hash; RULE_HASH]);
    Item {
        snapshot: Some(SnapshotRecord::for_test(
            [hash; RULE_HASH],
            Snapshot::new(&format!("{{\"rule\":{hash}}}")).unwrap(),
            ts,
        )),
        event,
    }
}

fn counts(conn: &Connection) -> (i64, i64, i64) {
    let n = |table: &str| {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    (n("events"), n("rules_seen"), n("gaps"))
}

/// The child: runs `scenario`, then kills itself. Without the variables (the
/// normal test run) it does nothing.
#[test]
fn crash_child() {
    let (Ok(scenario), Ok(state)) = (std::env::var(SCENARIO), std::env::var(STATE)) else {
        return;
    };
    let location = locate(Mode::User, Some(std::path::Path::new(&state))).unwrap();
    let mut engine = engine_at(&location);
    let (_, clock) = manual_clock(1000);
    let registry = PinRegistry::with_clock(clock);
    let limits = LedgerLimits::from_budget(engine.budget());
    engine.begin_session(&registry, &limits).unwrap();
    engine
        .insert_batch(&[item(10, 1), item(20, 2), item(30, 3)])
        .unwrap();
    match scenario.as_str() {
        // Rows, their orphaned snapshots and the marker are all written inside
        // the transaction; the kill comes before COMMIT.
        "mid-chunk" => {
            engine.fault_before_commit_for_test(Box::new(|| kill_self()));
            let _ = engine.evict_chunk();
        }
        // The chunk commits, then the process dies.
        "after-chunk" => {
            engine.evict_chunk().unwrap();
        }
        // A batch finds no room: its pin is held; a gap and a heartbeat still
        // write from the reserve.
        "paused" => {
            let conn = engine.connection();
            let mut ts = 100;
            while engine.used_pages().unwrap() <= engine.budget().high_water_pages() + 200 {
                conn.execute_batch("BEGIN IMMEDIATE").unwrap();
                for _ in 0..32 {
                    ts += 1;
                    let mut e = EventRecord::new(ts, Action::Allow, AnsweredBy::Rule);
                    e.process_path = Some(super::caps::ProcessPath::new(&format!("/p{}", ts % 40)));
                    insert_event(conn, &e).unwrap();
                }
                conn.execute_batch("COMMIT").unwrap();
            }
            let batch = PinnedBatch {
                items: vec![item(2000, 4)],
                pins: vec![registry.accept().unwrap()],
            };
            assert!(engine.commit_pinned(&batch, &registry).is_err());
            engine.heartbeat(&registry).unwrap();
        }
        // The delete of an eviction-sized chunk has spilled dirty pages into
        // the main file (a tiny page cache with spilling on, on a second
        // connection: the engine itself turns spilling off, so this is the
        // state a build without that guard could reach), and the process
        // dies before COMMIT.
        "spilled-chunk" => {
            fill_events(&engine, 400);
            record_events(&engine, &location);
            spill_then_die(&location, 300);
        }
        // The same, from a store filled until eviction cannot make room.
        "spilled-paused" => {
            fill_until_pressed(&engine);
            record_events(&engine, &location);
            spill_then_die(&location, 4096);
        }
        other => panic!("unknown scenario {other}"),
    }
    kill_self();
}

/// `n` raw rows in one committed transaction.
fn fill_events(engine: &super::writer::Engine, n: i64) {
    let conn = engine.connection();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    for ts in 1..=n {
        let mut e = EventRecord::new(100 + ts, Action::Allow, AnsweredBy::Rule);
        e.process_path = Some(super::caps::ProcessPath::new(&format!("/p{}", ts % 40)));
        insert_event(conn, &e).unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
}

fn fill_until_pressed(engine: &super::writer::Engine) {
    while engine.used_pages().unwrap() <= engine.budget().high_water_pages() + 200 {
        fill_events(engine, 32);
    }
}

fn precount_path(location: &Location) -> std::path::PathBuf {
    location.db_path().with_extension("precount")
}

/// Tells the parent how many events there were before the kill.
fn record_events(engine: &super::writer::Engine, location: &Location) {
    let n = counts(engine.connection()).0;
    std::fs::write(precount_path(location), n.to_string()).unwrap();
}

/// Deletes the `n` oldest events on a connection that spills, checks the
/// journal is on disk with the pages the delete touched, and returns with the
/// transaction open.
fn spill_then_die(location: &Location, n: i64) -> ! {
    let conn = Connection::open(location.db_path()).unwrap();
    conn.execute_batch("PRAGMA cache_size = 2; PRAGMA cache_spill = ON; BEGIN IMMEDIATE;")
        .unwrap();
    conn.execute(
        "DELETE FROM events WHERE id IN (SELECT id FROM events ORDER BY ts_ms, id LIMIT ?1)",
        [n],
    )
    .unwrap();
    let mut journal = location.db_path().into_os_string();
    journal.push("-journal");
    let len = std::fs::metadata(&journal).unwrap().len();
    assert!(len > 512, "the delete's pages are in the journal: {len}");
    kill_self()
}

/// Runs `scenario` in a child that is killed; the state it left.
fn crash(scenario: &str) -> (tempfile::TempDir, Location) {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let consent = Consent {
        text_id: "t".into(),
        accepted_ms: 1,
    };
    enable(&location, consent).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "history::retention_crash_tests::crash_child",
            "--test-threads=1",
        ])
        .env(SCENARIO, scenario)
        .env(STATE, &state)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(9), "the child must die by SIGKILL");
    (root, location)
}

fn integrity(conn: &Connection) {
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
}

#[test]
fn kill_9_in_the_middle_of_an_eviction_chunk_leaves_rows_snapshots_and_ledger_as_they_were() {
    let (_root, location) = crash("mid-chunk");
    let engine = engine_at(&location); // recovers the hot journal
    assert_eq!(
        counts(engine.connection()),
        (3, 3, 0),
        "no rows gone, no marker"
    );
    integrity(engine.connection());
}

#[test]
fn kill_9_right_after_a_chunk_leaves_the_delete_and_its_marker_together() {
    let (_root, location) = crash("after-chunk");
    let engine = engine_at(&location);
    let conn = engine.connection();
    assert_eq!(counts(conn).0, 0);
    assert_eq!(
        counts(conn).1,
        0,
        "the orphaned snapshots went with the rows"
    );
    let evicted: Vec<Gap> = read_gaps(conn)
        .unwrap()
        .into_iter()
        .filter(|g| g.cause == GapCause::Evicted)
        .collect();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].count, 3);
    for ts in [10, 20, 30] {
        assert!(
            evicted[0].start_ms <= ts && ts <= evicted[0].end_ms,
            "{ts} is covered"
        );
    }
    integrity(conn);
}

#[test]
fn kill_9_during_a_pause_keeps_the_file_in_budget_and_the_lost_batchs_gap_begins_at_its_pin() {
    let (_root, location) = crash("paused");
    let mut engine = engine_at(&location);
    integrity(engine.connection());
    assert!(engine.file_pages().unwrap() <= engine.budget().max_page_count);
    assert!(read_watermark(engine.connection()).unwrap().is_some());
    let registry = PinRegistry::with_clock(Arc::new(|| BIG));
    let limits = LedgerLimits::from_budget(&Budget::new(16 * 1024 * 1024).unwrap());
    let gap = engine
        .begin_session(&registry, &limits)
        .unwrap()
        .expect("a crash leaves a gap");
    assert!(gap.is_uncertain() && gap.cause == GapCause::BridgeDown);
    assert_eq!(gap.end_ms, BIG);
    assert!(
        gap.start_ms <= 1000,
        "begins no later than the session: {gap:?}"
    );
}

fn assert_rolled_back(scenario: &str) {
    let (_root, location) = crash(scenario);
    let before: i64 = std::fs::read_to_string(precount_path(&location))
        .unwrap()
        .parse()
        .unwrap();
    let engine = engine_at(&location); // recovers the hot journal
    integrity(engine.connection());
    assert_eq!(
        counts(engine.connection()).0,
        before,
        "the spilled delete rolled back whole"
    );
    assert!(engine.file_pages().unwrap() <= engine.budget().max_page_count);
}

#[test]
fn kill_9_after_a_chunk_delete_spilled_to_the_main_file_rolls_back_whole() {
    assert_rolled_back("spilled-chunk");
}

#[test]
fn kill_9_after_a_delete_spilled_in_a_full_store_rolls_back_whole() {
    assert_rolled_back("spilled-paused");
}
