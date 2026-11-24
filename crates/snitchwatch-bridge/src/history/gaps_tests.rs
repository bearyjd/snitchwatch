//! PR 7: every cause of a gap becomes a durable row, W1 holds while it does,
//! and no interval is left without a row or an explicit statement.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection as Db;
use snitchwatch_proto::protocol::{Connection, Event, Rule};

use super::super::capture::{Capture, CaptureConfig};
use super::super::durable::tests::{engine_at, manual_clock, rig, Rig};
use super::super::ledger::{read_gaps, Gap, GapCause, LedgerLimits, LossCounter};
use super::super::pins::PinRegistry;
use super::super::session::{read_sessions, read_watermark, SessionEnd};
use super::super::test_support::{state_dir, tempdir};
use super::super::writer::WriteError;
use super::super::{enable, locate, Consent, Location, Mode};
use super::*;

fn losses() -> Mutex<Losses> {
    Mutex::new(Losses::new())
}

fn gaps(rig: &Rig) -> Vec<Gap> {
    read_gaps(rig.engine.connection()).unwrap()
}

fn beat_at(rig: &mut Rig, losses: &Mutex<Losses>, clock: &mut ClockWatch, mono: i64) {
    beat(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        losses,
        clock,
        mono,
    )
    .unwrap();
}

fn fail_once() -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let mut armed = true;
    Box::new(move || {
        std::mem::take(&mut armed).then(|| WriteError::Io(std::io::Error::other("injected")))
    })
}

fn gap(start_ms: i64, end_ms: i64, cause: GapCause, count: u64) -> Gap {
    Gap {
        start_ms,
        end_ms,
        start_exact: true,
        cause,
        count,
    }
}

// ---- 1. queue overflow and admission drop flush on the heartbeat --------

#[test]
fn queue_overflow_and_admission_drop_flush_to_gap_rows_on_heartbeat() {
    let mut rig = rig(1000);
    let losses = losses();
    losses
        .lock()
        .unwrap()
        .queue
        .record(&rig.registry, 3)
        .unwrap();
    rig.set(1200);
    losses
        .lock()
        .unwrap()
        .admission
        .record(&rig.registry, 2)
        .unwrap();
    rig.set(2000);
    beat_at(&mut rig, &losses, &mut ClockWatch::new(), 0);
    assert_eq!(
        gaps(&rig),
        vec![
            gap(1000, 2000, GapCause::QueueOverflow, 3),
            gap(1200, 2000, GapCause::AdmissionDrop, 2),
        ]
    );
    assert!(rig.registry.is_empty(), "the pins went with the rows");
    assert_eq!(losses.lock().unwrap().queue.pending_count(), 0);
}

#[test]
fn a_flush_that_fails_keeps_the_tally_and_its_pin_until_a_retry_writes_both() {
    let mut rig = rig(1000);
    let losses = losses();
    losses
        .lock()
        .unwrap()
        .queue
        .record(&rig.registry, 3)
        .unwrap();
    rig.set(2000);
    rig.engine.fault_before_commit_for_test(fail_once());
    let failed = beat(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        &losses,
        &mut ClockWatch::new(),
        0,
    );
    assert!(failed.is_err());
    assert!(gaps(&rig).is_empty(), "the row did not commit");
    assert_eq!(rig.registry.len(), 1, "so its pin is still held");
    assert_eq!(losses.lock().unwrap().queue.pending_count(), 3);
    rig.set(3000);
    beat_at(&mut rig, &losses, &mut ClockWatch::new(), 0);
    assert_eq!(
        gaps(&rig),
        vec![gap(1000, 3000, GapCause::QueueOverflow, 3)]
    );
    assert!(rig.registry.is_empty());
}

#[test]
fn a_loss_noticed_while_a_flush_runs_joins_the_tally_that_comes_back() {
    let rig = rig(1000);
    let mut counter = LossCounter::new(GapCause::QueueOverflow);
    counter.record(&rig.registry, 3).unwrap();
    let taken = counter.take();
    rig.set(1500);
    counter.record(&rig.registry, 2).unwrap(); // noticed mid-flush
    counter.absorb(&rig.registry, taken); // the flush failed
    assert_eq!(counter.pending_count(), 5);
    assert_eq!(rig.registry.len(), 1, "one tally, one pin: the older one");
    assert_eq!(rig.registry.oldest(), Some(1000));
    let (gap, _) = counter.pending_gap(2000).unwrap();
    assert_eq!(gap.start_ms, 1000);
}

/// `flush_slot` runs its commit with the losses lock free. A loss counted
/// meanwhile must neither be written into the flushed row nor be lost.
#[test]
fn a_loss_counted_during_a_successful_flush_stays_for_the_next_one() {
    let mut rig = rig(1000);
    let (ms, read) = manual_clock(1000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let losses = Arc::new(losses());
    losses.lock().unwrap().queue.record(&registry, 3).unwrap();
    ms.store(2000, Ordering::SeqCst);
    let (mid_flush, mid_registry, mut armed) = (losses.clone(), registry.clone(), true);
    rig.engine.set_observer(Box::new(move |_| {
        if std::mem::take(&mut armed) {
            mid_flush
                .lock()
                .unwrap()
                .queue
                .record(&mid_registry, 2)
                .unwrap();
        }
    }));
    flush_slot(
        &mut rig.engine,
        &registry,
        &rig.limits,
        &losses,
        Slot::Queue,
        2000,
    )
    .unwrap();
    assert_eq!(
        gaps(&rig),
        vec![gap(1000, 2000, GapCause::QueueOverflow, 3)]
    );
    assert_eq!(losses.lock().unwrap().queue.pending_count(), 2);
    assert_eq!(registry.len(), 1, "the new loss holds its own pin");
}

#[test]
fn a_loss_counted_during_a_failed_flush_joins_the_tally_that_comes_back() {
    let mut rig = rig(1000);
    let (ms, read) = manual_clock(1000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let losses = Arc::new(losses());
    losses.lock().unwrap().queue.record(&registry, 3).unwrap();
    ms.store(2000, Ordering::SeqCst);
    let (mid_flush, mid_registry, mut armed) = (losses.clone(), registry.clone(), true);
    rig.engine.fault_before_commit_for_test(Box::new(move || {
        std::mem::take(&mut armed).then(|| {
            mid_flush
                .lock()
                .unwrap()
                .queue
                .record(&mid_registry, 2)
                .unwrap();
            WriteError::Io(std::io::Error::other("injected"))
        })
    }));
    let failed = flush_slot(
        &mut rig.engine,
        &registry,
        &rig.limits,
        &losses,
        Slot::Queue,
        2000,
    );
    assert!(failed.is_err());
    assert!(gaps(&rig).is_empty());
    assert_eq!(losses.lock().unwrap().queue.pending_count(), 5);
    assert_eq!(registry.len(), 1, "one tally, the older pin");
    assert_eq!(registry.oldest(), Some(1000));
}

// ---- 3. daemon-disconnected ---------------------------------------------

#[test]
fn a_daemon_disconnect_opens_at_the_down_edge_and_closes_at_recovery() {
    let mut rig = rig(1000);
    let losses = losses();
    let mut clock = ClockWatch::new();
    rig.set(2000);
    losses.lock().unwrap().daemon_down(&rig.registry);
    rig.set(3000);
    beat_at(&mut rig, &losses, &mut clock, 0);
    assert!(gaps(&rig).is_empty(), "still down: the gap is open");
    assert_eq!(rig.registry.len(), 1, "an open gap holds the watermark");
    losses.lock().unwrap().daemon_up(4000);
    rig.set(4500);
    beat_at(&mut rig, &losses, &mut clock, 0);
    assert_eq!(
        gaps(&rig),
        vec![gap(2000, 4000, GapCause::DaemonDisconnected, 1)]
    );
    assert!(rig.registry.is_empty());
}

#[test]
fn a_disconnect_still_open_at_shutdown_ends_at_the_shutdown() {
    let mut rig = rig(1000);
    let losses = losses();
    losses.lock().unwrap().daemon_down(&rig.registry);
    rig.set(5000);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, true).unwrap();
    assert_eq!(
        gaps(&rig),
        vec![gap(1000, 5000, GapCause::DaemonDisconnected, 1)]
    );
}

/// The daemon comes back while a closing flush of its open gap is in flight
/// and that flush fails: the gap must come back closed at that time, not
/// open (a heartbeat leaves an open gap alone, so it would never be written).
#[test]
fn a_daemon_up_during_a_failed_flush_closes_the_gap_that_comes_back() {
    let mut rig = rig(1000);
    let (ms, read) = manual_clock(1000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let losses = Arc::new(losses());
    ms.store(2000, Ordering::SeqCst);
    losses.lock().unwrap().daemon_down(&registry);
    ms.store(3000, Ordering::SeqCst);
    let (mid_flush, mut armed) = (losses.clone(), true);
    rig.engine.fault_before_commit_for_test(Box::new(move || {
        std::mem::take(&mut armed).then(|| {
            mid_flush.lock().unwrap().daemon_up(2500);
            WriteError::Io(std::io::Error::other("injected"))
        })
    }));
    let failed = flush_due(&mut rig.engine, &registry, &rig.limits, &losses, true);
    assert!(failed.is_err());
    // A heartbeat (not closing) writes it only if the gap is closed.
    ms.store(4000, Ordering::SeqCst);
    flush_due(&mut rig.engine, &registry, &rig.limits, &losses, false).unwrap();
    assert_eq!(
        gaps(&rig),
        vec![gap(2000, 2500, GapCause::DaemonDisconnected, 1)]
    );
    assert!(registry.is_empty());
}

// ---- 4. upstream loss ----------------------------------------------------

fn lost(missing: u64, restarted: bool, since_ms: i64) -> UpstreamLoss {
    UpstreamLoss {
        missing,
        restarted,
        since_ms: Some(since_ms),
        at_ms: None,
    }
}

#[test]
fn upstream_loss_is_the_lower_bound_sum_and_a_restart_adds_one_uncertain() {
    let mut rig = rig(1000);
    let losses = losses();
    losses
        .lock()
        .unwrap()
        .upstream_loss(&rig.registry, lost(4, false, 900));
    rig.set(1600);
    losses
        .lock()
        .unwrap()
        .upstream_loss(&rig.registry, lost(0, true, 1500));
    rig.set(2000);
    beat_at(&mut rig, &losses, &mut ClockWatch::new(), 0);
    assert_eq!(
        gaps(&rig),
        vec![Gap {
            start_ms: 900,
            end_ms: 2000,
            start_exact: false,
            cause: GapCause::UpstreamLoss,
            count: 5
        }]
    );
    assert!(gaps(&rig)[0].is_uncertain());
}

#[test]
fn a_ping_that_lost_nothing_makes_no_tally_and_holds_one_guard_pin_until_the_stop() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    for at in [1100, 1200, 1300] {
        rig.set(at);
        losses.lock().unwrap().upstream_loss(
            &rig.registry,
            UpstreamLoss {
                at_ms: Some(at),
                ..UpstreamLoss::default()
            },
        );
    }
    losses
        .lock()
        .unwrap()
        .upstream_loss(&rig.registry, lost(0, false, 900));
    assert_eq!(losses.lock().unwrap().upstream.pending_count(), 0);
    assert_eq!(rig.registry.len(), 1, "only the last ping is held");
    assert_eq!(rig.registry.oldest(), Some(1300));
    close(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        &losses,
        SessionEnd::Clean,
    )
    .unwrap();
    assert!(rig.registry.is_empty());
    assert_eq!(
        read_sessions(rig.engine.connection()).unwrap()[0].end,
        Some(SessionEnd::Clean)
    );
}

// ---- 6. clock jumps --------------------------------------------------------

fn jump(watch: &mut ClockWatch, wall: i64, mono: i64) -> Option<Gap> {
    watch.observe(wall, mono);
    let found = watch.unrecorded();
    watch.recorded();
    found
}

#[test]
fn a_clock_that_keeps_time_is_no_jump_and_the_threshold_is_strict() {
    let mut watch = ClockWatch::new();
    assert_eq!(jump(&mut watch, 10_000, 0), None, "no past to compare");
    assert_eq!(jump(&mut watch, 15_000, 5_000), None);
    // Exactly the threshold more than the monotonic clock: not a jump.
    assert_eq!(jump(&mut watch, 15_000 + 5_000 + 60_000, 10_000), None);
}

#[test]
fn a_backwards_step_is_a_clock_jump_covering_the_stepped_over_time() {
    let mut watch = ClockWatch::new();
    jump(&mut watch, 100_000, 0);
    let found = jump(&mut watch, 99_000, 5_000).expect("backwards");
    assert_eq!(found, gap(99_000, 100_000, GapCause::ClockJump, 1));
}

#[test]
fn a_step_back_smaller_than_the_heartbeat_interval_is_still_a_clock_jump() {
    let mut watch = ClockWatch::new();
    jump(&mut watch, 100_000, 0);
    // A 5 s heartbeat, the wall clock stepped back 3 s: it still moved
    // forward (2 s) but 3 s less than the monotonic clock did.
    assert!(
        jump(&mut watch, 102_000, 5_000).is_some(),
        "a 3 s step back is a jump"
    );
}

#[test]
fn a_slew_within_the_tolerance_is_no_clock_jump() {
    let mut watch = ClockWatch::new();
    jump(&mut watch, 100_000, 0);
    // NTP slewing: 0.5 s slow over a 5 s heartbeat.
    assert_eq!(jump(&mut watch, 104_500, 5_000), None);
    assert_eq!(jump(&mut watch, 109_000 + 500, 10_000), None);
}

#[test]
fn a_forward_jump_past_the_threshold_is_a_clock_jump() {
    let mut watch = ClockWatch::new();
    jump(&mut watch, 100_000, 0);
    let found = jump(&mut watch, 100_000 + 5_000 + 60_001, 5_000).expect("forward");
    assert_eq!(found, gap(100_000, 165_001, GapCause::ClockJump, 1));
}

#[test]
fn the_heartbeat_writes_a_clock_jump_row_and_keeps_it_until_it_is_durable() {
    let mut rig = rig(100_000);
    let losses = losses();
    let mut clock = ClockWatch::new();
    beat_at(&mut rig, &losses, &mut clock, 0);
    rig.set(90_000);
    rig.engine.fault_before_commit_for_test(fail_once());
    let failed = beat(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        &losses,
        &mut clock,
        5_000,
    );
    assert!(failed.is_err());
    assert!(gaps(&rig).is_empty());
    rig.set(90_500);
    beat_at(&mut rig, &losses, &mut clock, 5_500);
    // The stepped-over time reaches back to 100 000, which is after `now`.
    assert_eq!(
        gaps(&rig),
        vec![gap(90_000, 90_500, GapCause::ClockJump, 1)]
    );
}

#[test]
fn a_clock_jump_row_never_ends_after_the_registry_now() {
    let mut rig = rig(500_000);
    let losses = losses();
    let mut clock = ClockWatch::new();
    beat_at(&mut rig, &losses, &mut clock, 0);
    // The clock was far ahead, then is set back: the stepped-over time up to
    // the old reading lies in the future.
    rig.set(400_000);
    beat_at(&mut rig, &losses, &mut clock, 5_000);
    let rows = gaps(&rig);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].cause, GapCause::ClockJump);
    assert!(rows[0].end_ms <= 400_000, "{rows:?}");
    assert!(rows[0].start_ms <= rows[0].end_ms);
}

#[test]
fn a_flapping_clock_is_one_merged_row_and_cannot_collapse_other_gaps() {
    let mut rig = rig(1_000_000);
    rig.limits = LedgerLimits {
        max_rows: 8,
        max_bytes: u64::MAX,
    };
    let losses = losses();
    let mut clock = ClockWatch::new();
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    beat_at(&mut rig, &losses, &mut clock, 0);
    // The clock flaps every 5 s heartbeat, and each heartbeat also records an
    // unrelated gap that merges with its own kind (so the clock-jump row is
    // never the latest one).
    for beat_no in 1..=40_i64 {
        let wall = if beat_no % 2 == 0 {
            1_000_000 + beat_no * 5_000
        } else {
            1_000_000 + beat_no * 5_000 - 200_000
        };
        rig.set(wall);
        rig.engine
            .record_evicted(900_000, 1, &rig.registry, &rig.limits)
            .unwrap();
        beat_at(&mut rig, &losses, &mut clock, beat_no * 5_000);
    }
    let rows = gaps(&rig);
    let jumps: Vec<_> = rows
        .iter()
        .filter(|g| g.cause == GapCause::ClockJump)
        .collect();
    assert_eq!(jumps.len(), 1, "one merged row, got {rows:?}");
    assert!(jumps[0].count > 1, "it counts the flaps: {jumps:?}");
    assert!(
        rows.iter().all(|g| g.cause != GapCause::Uncertain),
        "nothing was collapsed: {rows:?}"
    );
    assert!(rows.iter().any(|g| g.cause == GapCause::Evicted));
}

// ---- evicted marker ------------------------------------------------------

#[test]
fn eviction_leaves_a_marker_with_the_earliest_remaining_time_and_the_count() {
    let mut rig = rig(1000);
    rig.set(9000);
    rig.engine
        .record_evicted(5000, 40, &rig.registry, &rig.limits)
        .unwrap();
    rig.engine
        .record_evicted(7000, 10, &rig.registry, &rig.limits)
        .unwrap();
    let found = gaps(&rig);
    assert_eq!(found.len(), 1, "adjacent markers merge");
    assert_eq!(found[0].cause, GapCause::Evicted);
    assert_eq!((found[0].end_ms, found[0].count), (7000, 50));
    assert!(found[0].is_uncertain(), "the start is whatever was removed");
}

// ---- running capture: write-failed, disable, labels, heartbeat -----------

const SHORT: Duration = Duration::from_millis(20);

struct Live {
    _root: tempfile::TempDir,
    location: Location,
    registry: Arc<PinRegistry>,
    clock: Arc<std::sync::atomic::AtomicI64>,
    capture: Capture,
}

fn live(
    heartbeat: Duration,
    fault: Option<Box<dyn FnMut() -> Option<WriteError> + Send>>,
    tweak: impl FnOnce(&mut CaptureConfig),
) -> Live {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let (clock, read) = manual_clock(50_000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let mut engine = engine_at(&location);
    if let Some(fault) = fault {
        engine.fault_before_commit_for_test(fault);
    }
    let mut config = CaptureConfig::for_budget(engine.budget());
    config.heartbeat_every = heartbeat;
    tweak(&mut config);
    let capture = Capture::spawn(engine, registry.clone(), config).unwrap();
    Live {
        _root: root,
        location,
        registry,
        clock,
        capture,
    }
}

fn db_gaps(location: &Location) -> Vec<Gap> {
    read_gaps(&Db::open(location.db_path()).unwrap()).unwrap()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn conn(host: &str) -> Connection {
    Connection {
        process_path: "/usr/bin/curl".into(),
        dst_host: host.into(),
        dst_port: 443,
        protocol: "tcp".into(),
        ..Default::default()
    }
}

fn rule(name: &str, nolog: bool) -> Rule {
    Rule {
        name: name.into(),
        action: "allow".into(),
        nolog,
        ..Default::default()
    }
}

fn event(host: &str, rule: Rule) -> Event {
    Event {
        connection: Some(conn(host)),
        rule: Some(rule),
        unixnano: 50_000 * 1_000_000,
        ..Default::default()
    }
}

/// Every commit after the first (the session start) fails.
fn disk_full_after_the_session_start() -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let commits = AtomicUsize::new(0);
    Box::new(move || {
        (commits.fetch_add(1, Ordering::SeqCst) >= 1)
            .then(|| WriteError::Io(std::io::Error::other("no space left on device")))
    })
}

#[test]
fn a_full_disk_counts_write_failed_and_keeps_the_pin_until_a_row_can_be_written() {
    let live = live(
        Duration::from_secs(3600),
        Some(disk_full_after_the_session_start()),
        |_| {},
    );
    live.capture
        .observe(&[&event("a.example", rule("r", false))]);
    wait_for("the failed batch", || live.capture.stats().lost_write == 1);
    wait_for("the tally", || {
        live.capture
            .with_losses_for_test(|l| l.write_failed.pending_count())
            == 1
    });
    assert_eq!(live.registry.oldest(), Some(50_000), "the pin is kept");
    let location = live.location.clone();
    let registry = live.registry.clone();
    live.capture.shutdown(); // its flush fails too: the disk is still full
    assert!(db_gaps(&location).is_empty());
    assert_eq!(registry.len(), 1, "still pinned: nothing was lost silently");
}

#[test]
fn a_write_failure_becomes_a_write_failed_gap_when_the_disk_recovers() {
    let commits = AtomicUsize::new(0);
    let fault: Box<dyn FnMut() -> Option<WriteError> + Send> = Box::new(move || {
        // 0: session start; 1 and 2: the batch and its retry.
        matches!(commits.fetch_add(1, Ordering::SeqCst), 1 | 2)
            .then(|| WriteError::Io(std::io::Error::other("injected")))
    });
    let live = live(Duration::from_secs(3600), Some(fault), |_| {});
    live.capture
        .observe(&[&event("a.example", rule("r", false))]);
    wait_for("the failed batch", || live.capture.stats().lost_write == 1);
    live.clock.store(60_000, Ordering::SeqCst);
    let (location, registry) = (live.location.clone(), live.registry.clone());
    live.capture.shutdown();
    assert_eq!(
        db_gaps(&location),
        vec![gap(50_000, 60_000, GapCause::WriteFailed, 1)]
    );
    assert!(registry.is_empty());
}

#[test]
fn the_running_writer_flushes_admission_drops_on_its_heartbeat() {
    let live = live(SHORT, None, |c| {
        c.admission.process_rate = 1;
        c.admission.process_burst = 1;
    });
    let events: Vec<Event> = (0..20)
        .map(|i| event(&format!("h{i}.example"), rule("r", false)))
        .collect();
    live.capture.observe(&events.iter().collect::<Vec<_>>());
    live.clock.store(51_000, Ordering::SeqCst);
    wait_for("the admission-drop row", || {
        db_gaps(&live.location)
            .iter()
            .any(|g| g.cause == GapCause::AdmissionDrop)
    });
    wait_for("the pin release", || live.registry.is_empty());
}

#[test]
fn disabling_mid_session_ends_it_as_disabled_and_the_next_start_records_the_gap() {
    let live = live(Duration::from_secs(3600), None, |_| {});
    live.clock.store(70_000, Ordering::SeqCst);
    let (location, registry) = (live.location.clone(), live.registry.clone());
    live.capture.disable();
    let sessions = read_sessions(engine_at(&location).connection()).unwrap();
    assert_eq!(sessions[0].end, Some(SessionEnd::Disabled));
    assert_eq!(sessions[0].end_ms, Some(70_000));
    // History is switched on again later.
    let later = Arc::new(PinRegistry::with_clock(Arc::new(|| 90_000)));
    let mut engine = engine_at(&location);
    let started = engine
        .begin_session(&later, &LedgerLimits::from_budget(engine.budget()))
        .unwrap();
    assert_eq!(
        started,
        Some(gap(70_000, 90_000, GapCause::Disabled, 1)),
        "exact bounds"
    );
    assert!(registry.is_empty());
}

#[test]
fn upstream_loss_reaches_a_gap_row_and_a_ws_lag_is_not_a_cause() {
    let live = live(Duration::from_secs(3600), None, |_| {});
    live.capture.observe_upstream_loss(lost(4, false, 40_000));
    live.clock.store(60_000, Ordering::SeqCst);
    let (location, registry) = (live.location.clone(), live.registry.clone());
    live.capture.shutdown();
    let found = db_gaps(&location);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].cause, GapCause::UpstreamLoss);
    assert_eq!((found[0].start_ms, found[0].count), (40_000, 4));
    assert!(registry.is_empty());
}

#[test]
fn a_daemon_disconnect_reaches_a_gap_row_through_the_taps() {
    let live = live(Duration::from_secs(3600), None, |_| {});
    live.clock.store(52_000, Ordering::SeqCst);
    live.capture.daemon_down();
    live.clock.store(58_000, Ordering::SeqCst);
    live.capture.daemon_up();
    live.clock.store(59_000, Ordering::SeqCst);
    let location = live.location.clone();
    live.capture.shutdown();
    assert_eq!(
        db_gaps(&location),
        vec![gap(52_000, 58_000, GapCause::DaemonDisconnected, 1)]
    );
}

// ---- 8. standing labels --------------------------------------------------

#[test]
fn standing_labels_are_present_only_when_they_apply() {
    let live = live(Duration::from_secs(3600), None, |_| {});
    let fresh = live.capture.standing_labels();
    assert_eq!(
        fresh,
        StandingLabels {
            nolog: false,
            stock_default_unobservable: true,
            no_gui_prompts: false
        }
    );
    live.capture
        .observe(&[&event("a.example", rule("quiet", true))]);
    live.capture.note_no_gui_prompt();
    let after = live.capture.standing_labels();
    assert!(after.nolog && after.no_gui_prompts);
    assert!(after.stock_default_unobservable, "no fork marker yet");
    let default = Event {
        rule: Some(Rule {
            name: String::new(),
            description: crate::daemon_contract::DEFAULT_ACTION_MARKER.into(),
            action: "allow".into(),
            ..Default::default()
        }),
        ..event("b.example", rule("x", false))
    };
    live.capture.observe(&[&default]);
    assert!(
        !live.capture.standing_labels().stock_default_unobservable,
        "a fork default-action event was seen"
    );
}

// ---- 9. no hole ----------------------------------------------------------

/// The parts of `[from, to]` no session and no gap covers.
fn holes(conn: &Db, from: i64, to: i64) -> Vec<(i64, i64)> {
    let watermark = read_watermark(conn).unwrap().unwrap_or(to);
    let mut spans: Vec<(i64, i64)> = read_sessions(conn)
        .unwrap()
        .iter()
        .map(|s| (s.start_ms, s.end_ms.unwrap_or(watermark).max(s.start_ms)))
        .collect();
    spans.extend(
        read_gaps(conn)
            .unwrap()
            .iter()
            .map(|g| (g.start_ms, g.end_ms)),
    );
    spans.sort_unstable();
    let mut covered = from;
    let mut found = Vec::new();
    for (start, end) in spans {
        if start > covered {
            found.push((covered, start));
        }
        covered = covered.max(end);
    }
    if covered < to {
        found.push((covered, to));
    }
    found
}

fn restart(rig: &mut Rig, at: i64) {
    rig.set(at);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
}

#[test]
fn no_interval_is_left_without_a_session_or_a_gap_after_every_scenario() {
    let mut rig = rig(1000);
    let losses = losses();
    let mut clock = ClockWatch::new();
    restart(&mut rig, 1000);
    // Losses of every cause, a disconnect, a clock step.
    losses
        .lock()
        .unwrap()
        .queue
        .record(&rig.registry, 2)
        .unwrap();
    rig.set(1500);
    losses.lock().unwrap().daemon_down(&rig.registry);
    rig.set(2500);
    losses.lock().unwrap().daemon_up(2400);
    losses
        .lock()
        .unwrap()
        .upstream_loss(&rig.registry, lost(3, true, 1200));
    beat_at(&mut rig, &losses, &mut clock, 1500);
    rig.set(2000);
    beat_at(&mut rig, &losses, &mut clock, 2500);
    // A clean stop, then a start; a crash (no end), then a start; a disable.
    rig.engine
        .end_session(&rig.registry, SessionEnd::Clean)
        .unwrap();
    restart(&mut rig, 4000);
    rig.set(4500);
    rig.engine.heartbeat(&rig.registry).unwrap();
    restart(&mut rig, 6000); // the crash
    rig.engine
        .end_session(&rig.registry, SessionEnd::Disabled)
        .unwrap();
    restart(&mut rig, 8000);
    assert_eq!(
        holes(rig.engine.connection(), 1000, 8000),
        Vec::<(i64, i64)>::new()
    );
}

#[test]
fn the_hole_check_finds_a_hole_when_there_is_one() {
    let mut rig = rig(1000);
    restart(&mut rig, 1000);
    rig.set(2000);
    rig.engine
        .end_session(&rig.registry, SessionEnd::Clean)
        .unwrap();
    rig.engine
        .connection()
        .execute("INSERT INTO sessions (start_ms) VALUES (5000)", [])
        .unwrap();
    assert_eq!(
        holes(rig.engine.connection(), 1000, 5000),
        vec![(2000, 5000)]
    );
}

// ---- 3. the watchdog's edges ----

/// History PR 7: the down edge opens a `daemon-disconnected` gap and
/// recovery closes it, at the times the edges were taken.
#[tokio::test(start_paused = true)]
async fn history_gets_the_down_and_up_edges_as_one_exact_gap() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let (ms, clock) = manual_clock(50_000);
    let engine = engine_at(&location);
    let mut config = crate::history::capture::CaptureConfig::for_budget(engine.budget());
    config.heartbeat_every = Duration::from_secs(3600);
    let capture = Arc::new(
        crate::history::capture::Capture::spawn(
            engine,
            Arc::new(PinRegistry::with_clock(clock)),
            config,
        )
        .unwrap(),
    );

    let liveness = crate::daemon_liveness::DaemonLiveness::new();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(tokio::sync::Mutex::new(
        crate::cache::connections::ConnectionCache::with_tray_publisher(64, tray_pub.clone()),
    ));
    let mut rx = tray_pub.subscribe();
    let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);
    let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
        liveness.clone(),
        Arc::new(Mutex::new(None)),
        Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok()),
        Arc::new(crate::daemon_alerts::DaemonAlertStore::new()),
    ));
    let watchdog = tokio::spawn(crate::daemon_watchdog::run_with_history(
        liveness.clone(),
        tray_pub,
        cache,
        diagnostics_ctx,
        broadcast_tx,
        Some(capture.clone()),
    ));

    ms.store(52_000, Ordering::SeqCst);
    tokio::time::advance(crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT + Duration::from_secs(1))
        .await;
    rx.changed().await.unwrap();
    assert_eq!(*rx.borrow(), crate::tray_state::TrayState::DaemonDown);
    ms.store(58_000, Ordering::SeqCst);
    liveness.touch();
    tokio::time::advance(Duration::from_secs(4)).await;
    rx.changed().await.unwrap();
    assert_eq!(*rx.borrow(), crate::tray_state::TrayState::Idle);

    watchdog.abort();
    let _ = watchdog.await;
    Arc::try_unwrap(capture)
        .ok()
        .expect("the watchdog is gone")
        .shutdown();
    let gaps = read_gaps(&rusqlite::Connection::open(location.db_path()).unwrap()).unwrap();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].cause, GapCause::DaemonDisconnected);
    assert_eq!((gaps[0].start_ms, gaps[0].end_ms), (52_000, 58_000));
    assert!(gaps[0].start_exact);
}
