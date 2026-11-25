//! PR 7, fourth repair pass: the holes an adversarial review found between a
//! loss, its pin, the watermark and the rows that cover it. Each test names
//! the failure it pins, and most end on a coverage-style assertion (the
//! watermark never past a live pin, no instant without a row).

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::durable::tests::{engine_at, rig, Rig};
use super::super::gap_counts::summarize;
use super::super::ledger::{read_gaps, Gap, GapCause};
use super::super::pins::PinRegistry;
use super::super::session::{read_watermark, SessionEnd};
use super::super::writer::WriteError;
use super::*;

fn losses() -> Mutex<Losses> {
    Mutex::new(Losses::new())
}

fn rows(rig: &Rig) -> Vec<Gap> {
    read_gaps(rig.engine.connection()).unwrap()
}

fn ping(missing: u64, since: Option<i64>, at: i64) -> UpstreamLoss {
    UpstreamLoss {
        missing,
        restarted: false,
        since_ms: since,
        at_ms: Some(at),
    }
}

fn beat_ok(rig: &mut Rig, losses: &Mutex<Losses>, clock: &mut ClockWatch, mono: i64) {
    let done = beat(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        losses,
        clock,
        mono,
    );
    assert!(done.is_ok(), "{done:?}");
}

fn fail_nth(nth: usize) -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let seen = AtomicUsize::new(0);
    Box::new(move || {
        (seen.fetch_add(1, Ordering::SeqCst) == nth)
            .then(|| WriteError::Io(std::io::Error::other("injected")))
    })
}

/// W1: the durable watermark never passes a live pin.
fn assert_watermark_held(rig: &Rig, what: &str) {
    let mark = read_watermark(rig.engine.connection()).unwrap();
    if let (Some(mark), Some(oldest)) = (mark, rig.registry.oldest()) {
        assert!(mark <= oldest, "{what}: watermark {mark} past pin {oldest}");
    }
}

/// Every instant in `[from, to]` lies in some row (or touches the next one).
fn assert_covered(rows: &[Gap], from: i64, to: i64, what: &str) {
    let mut sorted: Vec<&Gap> = rows.iter().collect();
    sorted.sort_by_key(|g| g.start_ms);
    let mut reached = from;
    for g in sorted {
        if g.start_ms <= reached {
            reached = reached.max(g.end_ms);
        }
    }
    assert!(reached >= to, "{what}: covered only to {reached}: {rows:?}");
}

/// What a `kill -9` at this point leaves: the next start at `restart_ms`
/// records the crashed session's gap from the durable watermark.
fn rows_after_a_crash(rig: &Rig, restart_ms: i64) -> Vec<Gap> {
    let mut next = engine_at(&rig.location);
    let registry = PinRegistry::with_clock(Arc::new(move || restart_ms));
    next.begin_session(&registry, &rig.limits).unwrap();
    read_gaps(next.connection()).unwrap()
}

// ---- 1. a clock that stepped back, against live pins -----------------------

#[test]
fn a_clock_step_back_never_leaves_the_watermark_past_a_live_pin() {
    let mut rig = rig(100_000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let (losses, mut clock) = (losses(), ClockWatch::new());
    rig.set(300_000);
    beat_ok(&mut rig, &losses, &mut clock, 0);
    rig.set(185_000); // the wall clock stepped back; a loss is noticed
    let _unflushed = rig.registry.accept().unwrap(); // a queued event
    rig.set(190_000);
    beat_ok(&mut rig, &losses, &mut clock, 5_000);
    assert_watermark_held(&rig, "after the step back");
    let after = rows_after_a_crash(&rig, 190_000);
    assert_covered(&after, 185_000, 190_000, "the loss at 185000");
}

// ---- 2. a failed post-outage tally -----------------------------------------

#[test]
fn a_failed_post_outage_tally_keeps_the_watermark_before_the_lost_stretch() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    {
        let mut l = locked(&losses);
        let first = l.begin_ping(&rig.registry, ping(0, None, 1000), 0);
        l.finish_ping(&rig.registry, first);
    }
    rig.set(2000);
    locked(&losses).daemon_down(&rig.registry);
    rig.set(4000);
    locked(&losses).daemon_up(4000);
    rig.set(4500);
    locked(&losses).upstream_loss(&rig.registry, ping(3, Some(1000), 4500));
    rig.set(5000);
    // The pre-outage row, the post-outage row and the daemon row are one
    // commit (#432), and it fails.
    rig.engine.fault_before_commit_for_test(fail_nth(0));
    let mut clock = ClockWatch::new();
    let failed = beat(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        &losses,
        &mut clock,
        0,
    );
    assert!(failed.is_err());
    assert_watermark_held(&rig, "after the failed tally");
    let mut all = rows(&rig);
    all.extend(rows_after_a_crash(&rig, 6000));
    assert_covered(&all, 1000, 4500, "nothing from the last ping to the first");
}

// ---- 3. a ping releases its predecessor's guard only after its events ------

#[test]
fn a_ping_pins_itself_before_it_releases_the_previous_guard() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let (losses, mut clock) = (losses(), ClockWatch::new());
    {
        let mut l = locked(&losses);
        let first = l.begin_ping(&rig.registry, ping(0, None, 1000), 0);
        l.finish_ping(&rig.registry, first);
    }
    rig.set(2000);
    let begun = locked(&losses).begin_ping(&rig.registry, ping(0, Some(1000), 2000), 1);
    assert_eq!(rig.registry.len(), 2, "its own guard first, the old kept");
    beat_ok(&mut rig, &losses, &mut clock, 0);
    assert_watermark_held(&rig, "events not yet queued");
    let held = read_watermark(rig.engine.connection()).unwrap();
    assert!(held <= Some(1000), "{held:?}: the 1500 event is not queued");
    let _event_pin = rig.registry.accept().unwrap(); // the events are queued
    locked(&losses).finish_ping(&rig.registry, begun);
    assert_eq!(
        rig.registry.len(),
        2,
        "the old guard went, the event's came"
    );
}

// ---- 4. overlapping pings --------------------------------------------------

#[test]
fn a_loss_starts_at_its_own_since_when_pings_settle_out_of_order() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    {
        let mut l = locked(&losses);
        let first = l.begin_ping(&rig.registry, ping(0, None, 1000), 0);
        l.finish_ping(&rig.registry, first);
    }
    rig.set(3000);
    let (a, b) = {
        let mut l = locked(&losses);
        (
            l.begin_ping(&rig.registry, ping(3, Some(1000), 2000), 0),
            l.begin_ping(&rig.registry, ping(0, Some(2000), 3000), 0),
        )
    };
    // B reaches history first.
    locked(&losses).finish_ping(&rig.registry, b);
    let mut clock = ClockWatch::new();
    beat_ok(&mut rig, &losses, &mut clock, 0);
    let held = read_watermark(rig.engine.connection()).unwrap();
    assert!(
        held <= Some(1000),
        "{held:?}: A's guard is still A's to use"
    );
    locked(&losses).finish_ping(&rig.registry, a);
    rig.set(4000);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    let found = rows(&rig);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].start_ms, found[0].count), (1000, 3), "{found:?}");
    assert_watermark_held(&rig, "after the flush");
    assert_eq!(rig.registry.oldest(), Some(3000), "only B's guard is left");
    assert_eq!(rig.registry.len(), 1);
}

// ---- 5. a second outage while the first is flushed -------------------------

#[test]
fn a_second_outage_during_a_flush_gets_its_own_row() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = Arc::new(losses());
    let clock = rig.clock.clone();
    let registry = Arc::new(PinRegistry::with_clock(Arc::new(move || {
        clock.load(Ordering::SeqCst)
    })));
    locked(&losses).daemon_down(&registry);
    rig.set(2000);
    locked(&losses).daemon_up(2000);
    rig.set(2500);
    let mid = losses.clone();
    let during = registry.clone();
    let mut armed = true;
    rig.engine.fault_before_commit_for_test(Box::new(move || {
        if std::mem::take(&mut armed) {
            locked(&mid).daemon_down(&during);
        }
        None
    }));
    flush_due(&mut rig.engine, &registry, &rig.limits, &losses, false).unwrap();
    assert_eq!(rows(&rig).len(), 1, "the first outage: [1000, 2000]");
    rig.set(3500);
    locked(&losses).daemon_up(3500);
    flush_due(&mut rig.engine, &registry, &rig.limits, &losses, false).unwrap();
    let found = rows(&rig);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(
        (found[1].start_ms, found[1].end_ms),
        (2500, 3500),
        "{found:?}"
    );
}

// ---- 6. a clock jump that could not be written yet --------------------------

fn jump_pending(rig: &mut Rig, losses: &Mutex<Losses>, clock: &mut ClockWatch) {
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    beat_ok(rig, losses, clock, 0);
    rig.set(500_000);
    rig.engine.fault_before_commit_for_test(fail_nth(0));
    let failed = beat(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        losses,
        clock,
        5_000,
    );
    assert!(failed.is_err(), "the jump's write failed");
}

#[test]
fn a_jump_whose_write_failed_holds_the_watermark_until_it_is_recorded() {
    let mut rig = rig(1000);
    let (losses, mut clock) = (losses(), ClockWatch::new());
    jump_pending(&mut rig, &losses, &mut clock);
    assert_watermark_held(&rig, "the jump is unrecorded");
    let held = read_watermark(rig.engine.connection()).unwrap();
    assert!(held <= Some(1000), "{held:?} passed the unrecorded jump");
    beat_ok(&mut rig, &losses, &mut clock, 10_000);
    assert!(rows(&rig).iter().any(|g| g.cause == GapCause::ClockJump));
    assert!(rig.registry.is_empty(), "recorded, so its pin went");
}

#[test]
fn a_clean_shutdown_records_the_jump_it_could_not_write_before() {
    let mut rig = rig(1000);
    let (losses, mut clock) = (losses(), ClockWatch::new());
    jump_pending(&mut rig, &losses, &mut clock);
    close(
        &mut rig.engine,
        &rig.registry,
        &rig.limits,
        &losses,
        &mut clock,
        SessionEnd::Clean,
    )
    .unwrap();
    let found = rows(&rig);
    assert!(
        found.iter().any(|g| g.cause == GapCause::ClockJump),
        "{found:?}"
    );
}

// ---- 8. the first ping back before the watchdog's poll ----------------------

#[test]
fn a_ping_back_before_the_watchdog_polls_still_splits_the_outage() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    rig.set(2000);
    locked(&losses).daemon_down(&rig.registry);
    rig.set(4500);
    locked(&losses).upstream_loss(&rig.registry, ping(3, Some(1000), 4500));
    rig.set(4600);
    locked(&losses).daemon_up(4600); // the watchdog's poll, after the ping
    rig.set(5000);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    let found = rows(&rig);
    for g in found.iter().filter(|g| g.cause == GapCause::UpstreamLoss) {
        assert!(g.end_ms <= 2000 || g.start_ms >= 4500, "{found:?}");
    }
    assert_covered(&found, 1000, 4500, "the outage and what lies around it");
}

// ---- 9. one reading of the clock for the down edge --------------------------

#[test]
fn the_rows_either_side_of_an_outage_abut_exactly() {
    let mut rig = rig(0);
    let ticks = Arc::new(AtomicI64::new(1000));
    let ticking = PinRegistry::with_clock(Arc::new(move || ticks.fetch_add(1, Ordering::SeqCst)));
    let losses = losses();
    locked(&losses).daemon_down(&ticking);
    locked(&losses).daemon_up(5000);
    locked(&losses).upstream_loss(&ticking, ping(3, Some(900), 6000));
    flush_due(&mut rig.engine, &ticking, &rig.limits, &losses, false).unwrap();
    let found = rows(&rig);
    let pre = found
        .iter()
        .find(|g| g.cause == GapCause::UpstreamLoss && g.end_ms <= 2000)
        .expect("pre-outage row");
    let daemon = found
        .iter()
        .find(|g| g.cause == GapCause::DaemonDisconnected)
        .expect("daemon row");
    assert_eq!(pre.end_ms, daemon.start_ms, "{found:?}");
}

// ---- 11. a jump before the first heartbeat ----------------------------------

#[test]
fn a_jump_before_the_first_heartbeat_is_flagged() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let mut pulse = Pulse::new(Duration::from_secs(1), 1000);
    rig.set(500_000);
    pulse
        .beat(&mut rig.engine, &rig.registry, &rig.limits, &losses())
        .unwrap();
    assert!(rows(&rig).iter().any(|g| g.cause == GapCause::ClockJump));
}

// ---- 13. a daemon that never connected --------------------------------------

#[tokio::test]
async fn a_daemon_that_never_connected_is_not_recorded_as_having_disconnected() {
    use crate::history::capture::{Capture, CaptureConfig};
    let rig = rig(50_000);
    let engine = engine_at(&rig.location);
    let mut config = CaptureConfig::for_budget(engine.budget());
    config.heartbeat_every = Duration::from_secs(3600);
    let capture = Arc::new(Capture::spawn(engine, Arc::new(PinRegistry::new()), config).unwrap());
    // A real, fresh liveness: its startup grace is not an observation.
    let liveness = crate::daemon_liveness::DaemonLiveness::new();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(tokio::sync::Mutex::new(
        crate::cache::connections::ConnectionCache::with_tray_publisher(64, tray_pub.clone()),
    ));
    let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);
    let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
        liveness.clone(),
        Arc::new(Mutex::new(None)),
        Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok()),
        Arc::new(crate::daemon_alerts::DaemonAlertStore::new()),
    ));
    let mut rx = tray_pub.subscribe();
    let watchdog = tokio::spawn(crate::daemon_watchdog::run_with_history_timing(
        liveness,
        tray_pub,
        cache,
        diagnostics_ctx,
        broadcast_tx,
        Some(capture.clone()),
        (Duration::from_millis(10), Duration::from_millis(60)),
    ));
    rx.changed().await.unwrap();
    assert_eq!(*rx.borrow(), crate::tray_state::TrayState::DaemonDown);
    assert!(
        !capture.daemon_gap_open_for_test(),
        "no daemon activity was ever seen: no outage began"
    );
    watchdog.abort();
}

// ---- a ping whose handler was dropped ---------------------------------------

#[test]
fn a_ping_cancelled_before_its_events_were_queued_counts_them_lost() {
    let rig = rig(1000);
    let mut l = Losses::new();
    let begun = l.begin_ping(&rig.registry, ping(0, None, 1000), 2);
    l.finish_ping(&rig.registry, begun.cancelled());
    assert_eq!(l.upstream.pending_count(), 2);
}

// ---- fifth pass -------------------------------------------------------------

/// M1: the end of the daemon gap and its tally are read under one lock. A
/// second outage that starts between the two (the seam runs there, with the
/// lock held; the outage can only land after the take) is a gap of its own.
#[test]
fn a_second_outage_cannot_slip_between_the_end_and_the_take() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = Arc::new(losses());
    let clock = rig.clock.clone();
    let registry = Arc::new(PinRegistry::with_clock(Arc::new(move || {
        clock.load(Ordering::SeqCst)
    })));
    locked(&losses).daemon_down(&registry);
    rig.set(2000);
    locked(&losses).daemon_up(2000);
    rig.set(2500);
    let handle = Arc::new(Mutex::new(None));
    let (mid, during, slot) = (losses.clone(), registry.clone(), handle.clone());
    locked(&losses).between_read_and_take = Some(Box::new(move || {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        let racer = std::thread::spawn(move || {
            locked(&mid).daemon_down(&during);
            flag.store(true, Ordering::SeqCst);
        });
        *slot.lock().unwrap() = Some(racer);
        // Lets the racer in if the lock is free; it is not, when the read
        // and the take share one critical section.
        for _ in 0..30 {
            if done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }));
    flush_due(&mut rig.engine, &registry, &rig.limits, &losses, false).unwrap();
    handle.lock().unwrap().take().unwrap().join().unwrap();
    assert_eq!(rows(&rig).len(), 1, "the first outage: [1000, 2000]");
    rig.set(3500);
    locked(&losses).daemon_up(3500);
    flush_due(&mut rig.engine, &registry, &rig.limits, &losses, false).unwrap();
    let found = rows(&rig);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(
        (found[1].start_ms, found[1].end_ms),
        (2500, 3500),
        "{found:?}"
    );
}

/// M2: B (since 2000) begins before A (since 1000, at 2000). B must not eat
/// the 1000 guard A needs: a `kill -9` after the heartbeat at 3000 leaves
/// every instant from 1000 covered.
#[test]
fn a_ping_that_begins_first_does_not_consume_an_earlier_pings_guard() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let (losses, mut clock) = (losses(), ClockWatch::new());
    {
        let mut l = locked(&losses);
        let first = l.begin_ping(&rig.registry, ping(0, None, 1000), 0);
        l.finish_ping(&rig.registry, first);
    }
    rig.set(3000);
    let (b, a) = {
        let mut l = locked(&losses);
        (
            l.begin_ping(&rig.registry, ping(0, Some(2000), 3000), 0),
            l.begin_ping(&rig.registry, ping(3, Some(1000), 2000), 0),
        )
    };
    assert!(a.previous.is_some(), "A still has the guard at 1000");
    beat_ok(&mut rig, &losses, &mut clock, 0);
    assert_watermark_held(&rig, "both pings in flight");
    let after = rows_after_a_crash(&rig, 3500);
    assert_covered(&after, 1000, 3500, "a crash with both pings in flight");
    locked(&losses).finish_ping(&rig.registry, b);
    locked(&losses).finish_ping(&rig.registry, a);
    assert_eq!(locked(&losses).upstream.pending_count(), 3);
}

/// L1: a ping whose since is below every guard key never takes a guard; the
/// guards must not pile up behind it.
#[test]
fn guards_do_not_pile_up_behind_pings_with_an_unmatched_since() {
    let rig = rig(1000);
    let mut l = Losses::new();
    for at in [1000, 2000, 3000, 4000] {
        let begun = l.begin_ping(&rig.registry, ping(0, Some(500), at), 0);
        l.finish_ping(&rig.registry, begun);
    }
    assert_eq!(rig.registry.len(), 1, "only the latest ping's guard");
    l.release_ping_guard(&rig.registry);
    assert_eq!(rig.registry.len(), 0, "close releases every guard");
}

/// L2: the watchdog's up edge after a ping already closed the outage leaves
/// no `back_at` behind to split the next ping's loss.
#[test]
fn a_late_watchdog_up_edge_leaves_no_stale_outage_end() {
    let rig = rig(1000);
    let mut l = Losses::new();
    l.daemon_down(&rig.registry);
    rig.set(4500);
    let begun = l.begin_ping(&rig.registry, ping(0, Some(1000), 4500), 0);
    assert!(begun.outage.is_some());
    l.finish_ping(&rig.registry, begun);
    l.daemon_up(4600); // the watchdog's poll, after the ping
    assert_eq!(l.back_at, None, "no outage is open for it to close");
    let next = l.begin_ping(&rig.registry, ping(2, Some(4500), 5000), 0);
    assert_eq!(next.outage, None, "the outage was closed once already");
}

// ---- 9. PR 7 sixth repair pass ----------------------------------------------

/// A crash after a clock rewind: the pin cap lowered the persisted watermark
/// below the session's own start, and recovery must start the gap there.
#[test]
fn a_crash_after_a_clock_rewind_starts_the_gap_at_the_repaired_watermark() {
    let mut rig = rig(1_000_000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let (losses, mut clock) = (losses(), ClockWatch::new());
    rig.set(995_000); // the wall clock stepped back
    let _queued = rig.registry.accept().unwrap(); // a pending prompt pins it
    rig.set(996_000);
    beat_ok(&mut rig, &losses, &mut clock, 1_000);
    let mark = read_watermark(rig.engine.connection()).unwrap();
    assert!(
        mark < Some(1_000_000),
        "the pin capped the watermark: {mark:?}"
    );
    let after = rows_after_a_crash(&rig, 1_009_000);
    assert_covered(&after, 995_000, 1_009_000, "from the repaired watermark");
}

/// A forward clock jump after the last heartbeat is seen by the close, so the
/// clean end is not written over an unrecorded jump.
#[test]
fn a_clean_stop_records_a_forward_jump_after_the_last_heartbeat() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    let mut pulse = Pulse::new(Duration::from_secs(1), 1000);
    rig.set(1000 + 3_600_000); // an hour forward, no heartbeat since
    pulse
        .close(
            &mut rig.engine,
            &rig.registry,
            &rig.limits,
            &losses,
            SessionEnd::Clean,
        )
        .unwrap();
    let found = rows(&rig);
    assert!(
        found.iter().any(|g| g.cause == GapCause::ClockJump),
        "{found:?}"
    );
}

/// The next ping must not release the pin the first ping's events still need.
#[test]
fn the_next_ping_leaves_a_ping_in_flights_only_pin() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let (losses, mut clock) = (losses(), ClockWatch::new());
    let a = locked(&losses).begin_ping(&rig.registry, ping(0, None, 1000), 0);
    rig.set(2000);
    {
        let mut l = locked(&losses);
        let b = l.begin_ping(&rig.registry, ping(0, Some(1000), 2000), 0);
        l.finish_ping(&rig.registry, b);
    }
    assert_eq!(rig.registry.oldest(), Some(1000), "A's pin is still live");
    rig.set(3000);
    beat_ok(&mut rig, &losses, &mut clock, 0);
    assert_watermark_held(&rig, "A still in flight");
    let after = rows_after_a_crash(&rig, 3500);
    assert_covered(&after, 1000, 3500, "a crash before A queued its events");
    locked(&losses).finish_ping(&rig.registry, a);
    assert_eq!(rig.registry.len(), 1, "then only B's guard remains");
}

// ---- #432 / #419: the outage's rows commit together ------------------------

/// An outage across which exactly one event was lost: the first ping after
/// the daemon came back reports `missing = 1`, which becomes a pre-outage
/// row, a post-outage row (count 1 each, #419) and the daemon row.
fn one_event_lost_across_an_outage(rig: &Rig, losses: &Mutex<Losses>) {
    {
        let mut l = locked(losses);
        let first = l.begin_ping(&rig.registry, ping(0, None, 1000), 0);
        l.finish_ping(&rig.registry, first);
    }
    rig.set(2000);
    locked(losses).daemon_down(&rig.registry);
    rig.set(4000);
    locked(losses).daemon_up(4000);
    rig.set(4500);
    locked(losses).upstream_loss(&rig.registry, ping(1, Some(1000), 4500));
    rig.set(5000);
}

/// A reader on its own connection, run before every commit: the largest
/// `lost_at_least` any read between the writer's commits could have shown.
fn worst_read_between_commits(rig: &mut Rig) -> Arc<AtomicUsize> {
    let worst = Arc::new(AtomicUsize::new(0));
    let (seen, db) = (worst.clone(), rig.location.db_path());
    rig.engine.fault_before_commit_for_test(Box::new(move || {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let read = summarize(&read_gaps(&conn).unwrap()).lost_at_least;
        seen.fetch_max(usize::try_from(read).unwrap(), Ordering::SeqCst);
        None
    }));
    worst
}

#[test]
fn one_lost_event_across_an_outage_never_reads_as_two_between_commits() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    one_event_lost_across_an_outage(&rig, &losses);
    let worst = worst_read_between_commits(&mut rig);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    assert_eq!(rows(&rig).len(), 3, "{:?}", rows(&rig));
    assert_eq!(summarize(&rows(&rig)).lost_at_least, 1, "n = 1 reads 1");
    assert!(
        worst.load(Ordering::SeqCst) <= 1,
        "a read between two commits overstated the loss"
    );
}

#[test]
fn a_failed_outage_commit_writes_none_of_its_rows_and_releases_no_pin() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    one_event_lost_across_an_outage(&rig, &losses);
    let pins = rig.registry.len();
    rig.engine.fault_before_commit_for_test(fail_nth(0));
    let failed = flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false);
    assert!(failed.is_err());
    assert!(rows(&rig).is_empty(), "all three or none: {:?}", rows(&rig));
    assert_eq!(rig.registry.len(), pins, "no pin released");
    assert_watermark_held(&rig, "after the failed group");
    // Every tally came back: the retry writes the whole outage at once.
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    let all = rows(&rig);
    assert_eq!(all.len(), 3, "{all:?}");
    assert_eq!(summarize(&all).lost_at_least, 1);
    assert!(rig.registry.len() < pins, "the pins were released");
}
