//! PR 8: the pause when eviction cannot make room (D5), the ledger reserve
//! that keeps gap and heartbeat writes alive meanwhile (owner decision
//! 2026-11-24, item 6), and the kill -9 cases of eviction and pause.

use std::time::{Duration, Instant};

use super::retention::Pause;

const T0_DELAYS_MS: [u64; 9] = [500, 1000, 2000, 4000, 8000, 16000, 32000, 60000, 60000];

#[test]
fn the_pause_doubles_to_the_cap_and_starts_over_after_a_success() {
    let mut pause = Pause::new(Duration::from_millis(500), Duration::from_secs(60));
    let t0 = Instant::now();
    assert!(!pause.active(t0));
    for ms in T0_DELAYS_MS {
        let delay = Duration::from_millis(ms);
        assert_eq!(pause.enter(t0), delay);
        assert!(pause.active(t0 + delay - Duration::from_millis(1)));
        assert!(!pause.active(t0 + delay), "it ends at the deadline");
    }
    assert_eq!(pause.enter(t0), Duration::from_secs(60), "capped");
    assert!(pause.reset(), "it had been paused");
    assert!(!pause.reset(), "and now is not");
    assert_eq!(pause.enter(t0), Duration::from_millis(500));
}

// ---- eviction that cannot make room ----------------------------------------------

use super::durable::PinnedBatch;
use super::ledger::{Gap, GapCause, LedgerLimits};
use super::pins::PinRegistry;
use super::record::{Action, AnsweredBy, EventRecord};
use super::retention_support::*;
use super::writer::{Item, WriteError, EVICTION_ROUNDS};

/// Tiny rows from many processes until the used pages are `over` past the
/// high-water mark: more than three eviction chunks can clear.
fn fill_to(rig: &Rig, over: u64) {
    let mut ts = 0;
    while used(rig) <= high_water(rig) + over {
        let rows: Vec<EventRecord> = (0..32)
            .map(|i| {
                ts += 1;
                event(ts, &format!("/usr/bin/p{}", (ts + i) % 40), Action::Allow)
            })
            .collect();
        fill(rig, rows);
    }
}

fn one_item(ts: i64) -> Item {
    plain(EventRecord::new(ts, Action::Allow, AnsweredBy::Rule))
}

#[test]
fn eviction_that_cannot_reach_high_water_within_the_chunk_limit_reports_not_made() {
    let mut rig = rig();
    fill_to(&rig, 200);
    let before = count(&rig, "events");
    assert!(!rig.engine.make_room().unwrap());
    let taken = before - count(&rig, "events");
    assert_eq!(
        taken,
        i64::from(EVICTION_ROUNDS) * 32,
        "it went on while chunks removed rows, and stopped at the limit"
    );
    assert!(used(&rig) > high_water(&rig));
    assert!(rig.engine.file_pages().unwrap() <= rig.engine.budget().max_page_count);
}

#[test]
fn a_batch_that_finds_no_room_fails_as_the_cap_and_keeps_its_pins() {
    let mut rig = rig();
    fill_to(&rig, 200);
    let registry = PinRegistry::new();
    let batch = PinnedBatch {
        items: vec![one_item(1_000_000)],
        pins: vec![registry.accept().unwrap()],
    };
    let events = count(&rig, "events");
    let err = rig.engine.commit_pinned(&batch, &registry).unwrap_err();
    assert!(
        matches!(err, super::durable::DurableError::Write(WriteError::Cap)),
        "{err:?}"
    );
    assert_eq!(registry.len(), 1, "W1: the pin is held until a row commits");
    assert!(count(&rig, "events") <= events, "nothing was inserted");
}

#[test]
fn gap_and_heartbeat_writes_keep_working_from_the_ledger_reserve_while_events_are_paused() {
    let mut rig = rig();
    // Events fill the file to the edge of the ledger's reserve (the pages the
    // budget keeps for it); nothing evicts enough to insert another.
    let reserve = rig.engine.budget().ledger_pages;
    let cap = rig.engine.budget().max_page_count;
    let mut ts = 0;
    while rig.engine.file_pages().unwrap() < cap - reserve {
        let rows: Vec<EventRecord> = (0..32)
            .map(|_| {
                ts += 1;
                event(ts, &format!("/usr/bin/p{}", ts % 40), Action::Allow)
            })
            .collect();
        fill(&rig, rows);
    }
    let registry = PinRegistry::new();
    let limits = LedgerLimits::from_budget(rig.engine.budget());
    let batch = PinnedBatch {
        items: vec![one_item(1)],
        pins: vec![registry.accept().unwrap()],
    };
    assert!(
        rig.engine.commit_pinned(&batch, &registry).is_err(),
        "events are paused"
    );
    // The ledger fills to its ceiling with rows that cannot merge.
    let causes = [
        GapCause::QueueOverflow,
        GapCause::AdmissionDrop,
        GapCause::WriteFailed,
    ];
    for i in 0..(limits.ceiling() as i64 + 20) {
        let gap = Gap {
            start_ms: i * 1_000,
            end_ms: i * 1_000 + 10,
            start_exact: true,
            cause: causes[i as usize % 3],
            count: 1,
        };
        rig.engine
            .commit_gap(&gap, &[], &registry, &limits)
            .unwrap_or_else(|e| panic!("gap {i} could not be written: {e}"));
        rig.engine.heartbeat(&registry).unwrap();
    }
    assert!(
        rig.engine.file_pages().unwrap() <= cap,
        "never past max_page_count"
    );
}

// ---- the running writer pauses ---------------------------------------------------

use std::sync::atomic::Ordering;
use std::sync::Arc;

use snitchwatch_proto::protocol::{Connection, Event, Rule};

use super::capture::{Capture, CaptureConfig};
use super::durable::tests::manual_clock;
use super::ledger::read_gaps;
use super::Location;

struct Live {
    _root: tempfile::TempDir,
    location: Location,
    registry: Arc<PinRegistry>,
    clock: Arc<std::sync::atomic::AtomicI64>,
    capture: Capture,
}

/// A capture over a store already `over` pages past high-water.
fn live(over: u64, tweak: impl FnOnce(&mut CaptureConfig)) -> Live {
    let rig = rig();
    fill_to(&rig, over);
    let Rig {
        _root,
        location,
        engine,
        ..
    } = rig;
    let (clock, read) = manual_clock(50_000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let mut config = CaptureConfig::for_budget(engine.budget());
    config.heartbeat_every = Duration::from_millis(20);
    tweak(&mut config);
    let capture = Capture::spawn(engine, registry.clone(), config).unwrap();
    Live {
        _root,
        location,
        registry,
        clock,
        capture,
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn web_event(host: &str) -> Event {
    Event {
        connection: Some(Connection {
            process_path: "/usr/bin/curl".into(),
            dst_host: host.into(),
            dst_port: 443,
            protocol: "tcp".into(),
            ..Default::default()
        }),
        rule: Some(Rule {
            name: "r".into(),
            action: "allow".into(),
            ..Default::default()
        }),
        unixnano: 50_000 * 1_000_000,
        ..Default::default()
    }
}

fn db_gaps(location: &Location) -> Vec<Gap> {
    read_gaps(&rusqlite::Connection::open(location.db_path()).unwrap()).unwrap()
}

fn db_events(location: &Location) -> i64 {
    rusqlite::Connection::open(location.db_path())
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn while_paused_events_are_lost_without_another_eviction_and_the_ledger_still_writes() {
    let live = live(200, |c| c.pause_min = Duration::from_secs(3600));
    let filled = db_events(&live.location);
    live.capture.observe(&[&web_event("a.example")]);
    wait_for("the first loss", || live.capture.stats().lost_cap == 1);
    assert!(live.capture.stats().cap_paused);
    let rows = db_events(&live.location);
    assert!(
        filled - rows > 0,
        "the first attempt did evict (a writer that never tries fails this)"
    );
    assert!(
        filled - rows <= i64::from(EVICTION_ROUNDS) * 32,
        "one attempt is at most the chunk limit, not a retry as well"
    );
    live.capture.observe(&[&web_event("b.example")]);
    wait_for("the second loss", || live.capture.stats().lost_cap == 2);
    assert_eq!(
        db_events(&live.location),
        rows,
        "a paused writer does not evict"
    );
    assert_eq!(live.capture.stats().written, 0);
    // The heartbeat flushes the losses into a gap row from the ledger reserve
    // while events stay paused, and releases their pins in that transaction.
    live.clock.store(100_000, Ordering::SeqCst); // under the clock-jump threshold
    wait_for("the write-failed row", || {
        db_gaps(&live.location)
            .iter()
            .any(|g| g.cause == GapCause::WriteFailed)
    });
    wait_for("the pin release", || live.registry.is_empty());
    let gaps = db_gaps(&live.location);
    let failed: Vec<&Gap> = gaps
        .iter()
        .filter(|g| g.cause == GapCause::WriteFailed)
        .collect();
    assert_eq!(failed.iter().map(|g| g.count).sum::<u64>(), 2, "{gaps:?}");
    assert_eq!(
        failed.iter().map(|g| g.start_ms).min(),
        Some(50_000),
        "from the first loss"
    );
    assert!(live.capture.stats().cap_paused);
    let location = live.location.clone();
    live.capture.shutdown();
    let conn = rusqlite::Connection::open(location.db_path()).unwrap();
    let pages: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    assert!(
        pages as u64
            <= super::budget::Budget::new(16 * 1024 * 1024)
                .unwrap()
                .max_page_count
    );
}

#[test]
fn recording_resumes_when_a_retry_makes_room_and_the_pause_state_clears() {
    // More than one attempt's chunks clear: the first attempt pauses, the retry
    // finishes.
    let live = live(120, |c| c.pause_min = Duration::from_millis(20));
    let mut sent = 0;
    wait_for("recording to resume", || {
        sent += 1;
        live.capture
            .observe(&[&web_event(&format!("h{sent}.example"))]);
        std::thread::sleep(Duration::from_millis(15));
        live.capture.stats().written > 0
    });
    let s = live.capture.stats();
    assert!(s.lost_cap >= 1, "the first attempt had no room: {s:?}");
    assert!(!s.cap_paused, "a success ends the pause: {s:?}");
}

// ---- the shutdown drain keeps evicting until its deadline passes ------------------

/// A closed, unstarted capture, an engine over high-water armed to stop as the
/// writer's would, and one pinned one-event batch.
fn closing(over: u64, deadline_in: Duration) -> (Capture, Rig, PinnedBatch, Arc<PinRegistry>) {
    let mut rig = rig();
    fill_to(&rig, over);
    let registry = Arc::new(PinRegistry::new());
    let config = CaptureConfig::for_budget(rig.engine.budget());
    let capture = Capture::new_unstarted_for_test(registry.clone(), config);
    let shared = capture.shared_for_test();
    shared.arm_stop(&mut rig.engine);
    shared.close_queue_for_test();
    shared.set_drain_deadline_for_test(Instant::now() + deadline_in);
    let batch = PinnedBatch {
        items: vec![one_item(1_000_000)],
        pins: vec![registry.accept().unwrap()],
    };
    (capture, rig, batch, registry)
}

#[test]
fn a_closed_queue_with_drain_time_left_still_evicts_for_and_writes_its_batch() {
    let (capture, mut rig, batch, registry) = closing(30, Duration::from_secs(60));
    assert!(used(&rig) > high_water(&rig));
    capture
        .shared_for_test()
        .write_for_test(&mut rig.engine, batch);
    let s = capture.stats();
    assert_eq!((s.written, s.lost_cap, s.lost_write), (1, 0, 0), "{s:?}");
    assert!(!s.cap_paused, "{s:?}");
    assert!(used(&rig) <= high_water(&rig));
    assert!(count(&rig, "events") > 0);
    assert!(registry.is_empty(), "the pin went with its row");
}

#[test]
fn a_passed_drain_deadline_loses_the_batch_as_a_plain_write_failure_not_the_cap() {
    let (capture, mut rig, batch, registry) = closing(200, Duration::ZERO);
    let before = remaining(&rig);
    capture
        .shared_for_test()
        .write_for_test(&mut rig.engine, batch);
    let s = capture.stats();
    assert_eq!((s.written, s.lost_cap, s.lost_write), (0, 0, 1), "{s:?}");
    assert!(!s.cap_paused, "no pause for a stop: {s:?}");
    let after = remaining(&rig);
    assert!(
        before.len() - after.len() <= 32,
        "at most one chunk, not a pass"
    );
    let gone: Vec<i64> = before
        .iter()
        .filter(|row| !after.contains(row))
        .map(|row| row.0)
        .collect();
    assert_removed_are_covered(&rig, &gone);
    assert_eq!(registry.len(), 1, "held until the tally is a row");
    let Rig {
        location, engine, ..
    } = rig;
    capture.shared_for_test().run_writer_for_test(engine);
    let failed: Vec<Gap> = db_gaps(&location)
        .into_iter()
        .filter(|g| g.cause == GapCause::WriteFailed)
        .collect();
    assert_eq!(failed.iter().map(|g| g.count).sum::<u64>(), 1, "{failed:?}");
    assert!(registry.is_empty(), "released with its row");
}
