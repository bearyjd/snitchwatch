//! W1 across a real `kill -9`: a child test process builds the state, then
//! SIGKILLs itself (no unwinding, no `Drop`, so no clean end is written); this
//! process reopens the file and reconstructs the gap. The child is this same
//! test binary, selected by an environment variable.

use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::Arc;

use super::budget::Budget;
use super::durable::tests::{engine_at, manual_clock};
use super::durable::PinnedBatch;
use super::gaps::Losses;
use super::ledger::{read_gaps, Gap, GapCause, LedgerLimits, LossCounter};
use super::pins::PinRegistry;
use super::record::{Action, AnsweredBy, EventRecord};
use super::session::{read_sessions, read_watermark, SessionEnd};
use super::test_support::{state_dir, tempdir};
use super::writer::Item;
use super::{enable, locate, Consent, Mode};
use crate::cache::rule_hits::UpstreamLoss;

const SCENARIO: &str = "SW_HISTORY_CRASH_SCENARIO";
const STATE: &str = "SW_HISTORY_CRASH_STATE";
const BIG: i64 = 9_000;

fn limits() -> LedgerLimits {
    LedgerLimits::from_budget(&Budget::new(16 * 1024 * 1024).unwrap())
}

fn event(ts: i64) -> Item {
    Item {
        event: EventRecord::new(ts, Action::Allow, AnsweredBy::Rule),
        snapshot: None,
    }
}

/// The child: runs `scenario`, then kills itself. Without the variable (the
/// normal test run) it does nothing.
#[test]
fn crash_child() {
    let (Ok(scenario), Ok(state)) = (std::env::var(SCENARIO), std::env::var(STATE)) else {
        return;
    };
    let location = locate(Mode::User, Some(std::path::Path::new(&state))).unwrap();
    let (ms, clock) = manual_clock(1000);
    let registry = PinRegistry::with_clock(clock);
    let mut engine = engine_at(&location);
    let limits = limits();
    engine.begin_session(&registry, &limits).unwrap();
    ms.store(2000, std::sync::atomic::Ordering::SeqCst);
    // What the process holds in memory when it dies.
    let _held_pin;
    let mut losses = Losses::new();
    let mut counter = LossCounter::new(GapCause::QueueOverflow);
    match scenario.as_str() {
        // (a) a queued daemon event: accepted, not yet written.
        "queued-event" => _held_pin = Some(registry.accept().unwrap()),
        // (b) a pending prompt: its guard holds a pin, other events commit around it.
        "pending-prompt" => {
            _held_pin = Some(registry.accept().unwrap());
            ms.store(2500, std::sync::atomic::Ordering::SeqCst);
            let batch = PinnedBatch {
                items: vec![event(2500)],
                pins: vec![registry.accept().unwrap()],
            };
            engine.commit_pinned(&batch, &registry).unwrap();
        }
        // (c) an unflushed drop counter.
        "drop-counter" => {
            counter.record(&registry, 7).unwrap();
            _held_pin = None;
        }
        // (d) an open daemon-disconnected gap, an unflushed upstream tally
        // and a write-failed tally: each holds its own pin.
        "open-disconnect" => {
            losses.daemon_down(&registry);
            _held_pin = None;
        }
        "upstream-tally" => {
            losses.upstream_loss(
                &registry,
                UpstreamLoss {
                    missing: 4,
                    restarted: false,
                    since_ms: Some(1500),
                },
            );
            _held_pin = None;
        }
        "write-failed-tally" => {
            losses.write_failed.record(&registry, 3).unwrap();
            _held_pin = None;
        }
        // Nothing held: the watermark is simply the last commit.
        "nothing-held" => _held_pin = None,
        // A counter that was flushed before the kill.
        "flushed-counter" => {
            counter.record(&registry, 7).unwrap();
            ms.store(2600, std::sync::atomic::Ordering::SeqCst);
            engine
                .flush_counter(&mut counter, &registry, &limits)
                .unwrap();
            _held_pin = None;
        }
        other => panic!("unknown scenario {other}"),
    }
    ms.store(4000, std::sync::atomic::Ordering::SeqCst);
    engine.heartbeat(&registry).unwrap(); // the commit after which the kill comes
                                          // SAFETY: kill(2) on our own pid; no memory is touched.
    unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    unreachable!("SIGKILL is not catchable");
}

/// Runs `scenario` in a child that is killed, then starts a new session in
/// this process at `BIG` and returns the reconstructed gap and the state.
fn crash_and_recover(scenario: &str) -> (Gap, super::Location) {
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
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "history::crash_tests::crash_child",
            "--test-threads=1",
        ])
        .env(SCENARIO, scenario)
        .env(STATE, &state)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(9), "the child must die by SIGKILL");
    let mut engine = engine_at(&location);
    let registry = PinRegistry::with_clock(Arc::new(|| BIG));
    let gap = engine
        .begin_session(&registry, &limits())
        .unwrap()
        .expect("a crash leaves a gap");
    std::mem::forget(root); // keep the files for the caller's follow-up reads
    (gap, location)
}

fn assert_uncertain_crash_gap(gap: &Gap, latest_start: i64) {
    assert!(gap.is_uncertain() && !gap.start_exact);
    assert_eq!(gap.cause, GapCause::BridgeDown);
    assert_eq!(gap.end_ms, BIG);
    assert!(
        gap.start_ms <= latest_start,
        "the gap begins at {} but must begin no later than {latest_start}",
        gap.start_ms
    );
    assert!(gap.start_ms >= 1000, "never before the session began");
}

#[test]
fn kill_9_with_a_queued_event_the_gap_begins_no_later_than_its_arrival() {
    let (gap, _) = crash_and_recover("queued-event");
    assert_uncertain_crash_gap(&gap, 2000);
}

#[test]
fn kill_9_with_a_pending_prompt_the_gap_begins_no_later_than_its_arrival() {
    let (gap, location) = crash_and_recover("pending-prompt");
    assert_uncertain_crash_gap(&gap, 2000);
    // The event committed around the prompt is durable.
    let engine = engine_at(&location);
    let events: i64 = engine
        .connection()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 1);
}

#[test]
fn kill_9_with_an_unflushed_drop_counter_the_gap_begins_no_later_than_its_first_loss() {
    let (gap, _) = crash_and_recover("drop-counter");
    assert_uncertain_crash_gap(&gap, 2000);
}

#[test]
fn kill_9_with_an_open_daemon_gap_leaves_an_uncertain_gap_from_the_down_edge() {
    let (gap, _) = crash_and_recover("open-disconnect");
    assert_uncertain_crash_gap(&gap, 2000);
}

#[test]
fn kill_9_with_an_unflushed_upstream_tally_the_gap_begins_no_later_than_its_pin() {
    let (gap, _) = crash_and_recover("upstream-tally");
    assert_uncertain_crash_gap(&gap, 2000);
}

#[test]
fn kill_9_with_an_unflushed_write_failed_tally_the_gap_begins_no_later_than_its_pin() {
    let (gap, _) = crash_and_recover("write-failed-tally");
    assert_uncertain_crash_gap(&gap, 2000);
}

#[test]
fn kill_9_with_nothing_held_the_gap_begins_at_the_last_commit() {
    let (gap, location) = crash_and_recover("nothing-held");
    assert_uncertain_crash_gap(&gap, 4000);
    assert_eq!(gap.start_ms, 4000, "no pin: the heartbeat's own time");
    let engine = engine_at(&location);
    let sessions = read_sessions(engine.connection()).unwrap();
    assert_eq!(sessions[0].end, Some(SessionEnd::Unclean));
    assert_eq!(sessions[0].end_ms, Some(4000));
    assert_eq!(read_watermark(engine.connection()).unwrap(), Some(BIG));
}

#[test]
fn a_counter_flushed_before_the_kill_keeps_its_gap_and_stops_holding_the_watermark() {
    let (gap, location) = crash_and_recover("flushed-counter");
    assert_eq!(gap.start_ms, 4000, "the released pin no longer holds it");
    let engine = engine_at(&location);
    let gaps = read_gaps(engine.connection()).unwrap();
    assert_eq!(gaps[0].cause, GapCause::QueueOverflow);
    assert_eq!((gaps[0].start_ms, gaps[0].count), (2000, 7));
}
