//! W1 on the durable path: the watermark is `min(commit, oldest pin)`, a pin
//! is released only by the transaction that made its item durable, and a
//! failed transaction releases nothing. The durable code takes an open store,
//! not a bridge mode: `Mode` matters only to `locate`, tested in `tests.rs`
//! for both modes.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use super::super::budget::Budget;
use super::super::ledger::{read_gaps, Gap, GapCause, LedgerLimits, LossCounter};
use super::super::pins::{Clock, PinRegistry};
use super::super::record::{Action, AnsweredBy, EventRecord};
use super::super::session::{read_sessions, read_watermark, Session, SessionEnd};
use super::super::test_support::{state_dir, tempdir};
use super::super::writer::{Engine, Item, WriteError};
use super::super::{enable, locate, open_store, Consent, Mode};
use super::*;

pub(crate) struct Rig {
    pub _root: tempfile::TempDir,
    pub location: super::super::Location,
    pub engine: Engine,
    pub clock: Arc<AtomicI64>,
    pub registry: PinRegistry,
    pub limits: LedgerLimits,
}

pub(crate) fn manual_clock(start: i64) -> (Arc<AtomicI64>, Clock) {
    let ms = Arc::new(AtomicI64::new(start));
    let read = ms.clone();
    (ms, Arc::new(move || read.load(Ordering::SeqCst)))
}

pub(crate) fn engine_at(location: &super::super::Location) -> Engine {
    let store = open_store(location).unwrap();
    let engine = Engine::new(store, location, Budget::new(16 * 1024 * 1024).unwrap()).unwrap();
    engine
        .connection()
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    engine
}

pub(crate) fn rig(start_ms: i64) -> Rig {
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
    let (clock, read) = manual_clock(start_ms);
    let engine = engine_at(&location);
    Rig {
        _root: root,
        location,
        engine,
        clock,
        registry: PinRegistry::with_clock(read),
        limits: LedgerLimits::from_budget(&Budget::new(16 * 1024 * 1024).unwrap()),
    }
}

impl Rig {
    pub fn set(&self, ms: i64) {
        self.clock.store(ms, Ordering::SeqCst);
    }

    fn count(&self, table: &str) -> i64 {
        self.engine
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn watermark(&self) -> Option<i64> {
        read_watermark(self.engine.connection()).unwrap()
    }

    fn gaps(&self) -> Vec<Gap> {
        read_gaps(self.engine.connection()).unwrap()
    }

    fn sessions(&self) -> Vec<Session> {
        read_sessions(self.engine.connection()).unwrap()
    }
}

fn item(ts: i64) -> Item {
    Item {
        event: EventRecord::new(ts, Action::Allow, AnsweredBy::Rule),
        snapshot: None,
    }
}

fn fail_once() -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let mut armed = true;
    Box::new(move || {
        std::mem::take(&mut armed).then(|| WriteError::Io(std::io::Error::other("injected")))
    })
}

fn pinned(rig: &Rig, ts: i64) -> PinnedBatch {
    PinnedBatch {
        items: vec![item(ts)],
        pins: vec![rig.registry.accept().unwrap()],
    }
}

#[test]
fn a_pin_is_released_in_the_transaction_that_wrote_its_event() {
    let mut rig = rig(1000);
    let batch = pinned(&rig, 1000);
    rig.set(1500);
    assert_eq!(rig.registry.len(), 1);
    rig.engine.commit_pinned(&batch, &rig.registry).unwrap();
    assert_eq!(rig.count("events"), 1);
    assert!(rig.registry.is_empty(), "the pin is released with the row");
    assert_eq!(rig.watermark(), Some(1500), "its own pin does not hold it");
}

#[test]
fn a_commit_that_fails_releases_no_pin_and_leaves_no_row() {
    let mut rig = rig(1000);
    let batch = pinned(&rig, 1000);
    rig.set(2000);
    rig.engine.fault_before_commit_for_test(fail_once());
    assert!(rig.engine.commit_pinned(&batch, &rig.registry).is_err());
    assert_eq!(rig.count("events"), 0, "the row did not commit");
    assert_eq!(rig.registry.len(), 1, "so its pin was not released");
    assert_eq!(rig.watermark(), None, "nor was the watermark written");
    // The retry succeeds and releases it.
    rig.engine.commit_pinned(&batch, &rig.registry).unwrap();
    assert_eq!(rig.count("events"), 1);
    assert!(rig.registry.is_empty());
}

#[test]
fn a_gap_commit_that_fails_releases_no_pin_and_leaves_no_row() {
    let mut rig = rig(1000);
    let pin = rig.registry.accept().unwrap();
    let gap = Gap {
        start_ms: 1000,
        end_ms: 1200,
        start_exact: true,
        cause: GapCause::QueueOverflow,
        count: 4,
    };
    rig.set(2000);
    rig.engine.fault_before_commit_for_test(fail_once());
    let failed = rig
        .engine
        .commit_gap(&gap, &[&pin], &rig.registry, &rig.limits);
    assert!(failed.is_err());
    assert!(rig.gaps().is_empty());
    assert_eq!(rig.registry.len(), 1);
    rig.engine
        .commit_gap(&gap, &[&pin], &rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(rig.gaps(), vec![gap]);
    assert!(rig.registry.is_empty());
}

#[test]
fn a_counter_is_flushed_with_its_pin_or_not_at_all() {
    let mut rig = rig(1000);
    let mut counter = LossCounter::new(GapCause::QueueOverflow);
    counter.record(&rig.registry, 2).unwrap();
    rig.set(1500);
    counter.record(&rig.registry, 3).unwrap();
    rig.set(2000);
    rig.engine.fault_before_commit_for_test(fail_once());
    assert!(rig
        .engine
        .flush_counter(&mut counter, &rig.registry, &rig.limits)
        .is_err());
    assert_eq!(counter.pending_count(), 5, "nothing was lost from memory");
    assert_eq!(rig.registry.len(), 1);
    assert!(rig.gaps().is_empty());

    assert!(rig
        .engine
        .flush_counter(&mut counter, &rig.registry, &rig.limits)
        .unwrap());
    assert_eq!(
        rig.gaps(),
        vec![Gap {
            start_ms: 1000,
            end_ms: 2000,
            start_exact: true,
            cause: GapCause::QueueOverflow,
            count: 5
        }]
    );
    assert_eq!(counter.pending_count(), 0);
    assert!(rig.registry.is_empty());
    assert!(!rig
        .engine
        .flush_counter(&mut counter, &rig.registry, &rig.limits)
        .unwrap());
}

#[test]
fn the_watermark_is_the_minimum_of_the_commit_and_the_oldest_pin() {
    let mut rig = rig(1000);
    let _prompt = rig.registry.accept().unwrap(); // a pending prompt, never settled here
    rig.set(3000);
    let batch = pinned(&rig, 3000);
    rig.set(5000);
    rig.engine.commit_pinned(&batch, &rig.registry).unwrap();
    assert_eq!(rig.watermark(), Some(1000), "the pending prompt holds it");
    rig.set(6000);
    rig.engine.heartbeat(&rig.registry).unwrap();
    assert_eq!(rig.watermark(), Some(1000));
}

#[test]
fn releasing_the_oldest_pin_lets_the_watermark_advance_past_it() {
    let mut rig = rig(1000);
    let old = rig.registry.accept().unwrap();
    rig.set(3000);
    let newer = rig.registry.accept().unwrap();
    rig.set(4000);
    let gap = Gap {
        start_ms: 1000,
        end_ms: 4000,
        start_exact: true,
        cause: GapCause::AdmissionDrop,
        count: 1,
    };
    rig.engine
        .commit_gap(&gap, &[&old], &rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(rig.watermark(), Some(3000), "held by the newer pin now");
    rig.engine
        .commit_gap(&gap, &[&newer], &rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(rig.watermark(), Some(4000));
}

#[test]
fn the_watermark_never_moves_backwards() {
    let mut rig = rig(5000);
    rig.engine.heartbeat(&rig.registry).unwrap();
    rig.set(4000); // the wall clock jumped back
    rig.engine.heartbeat(&rig.registry).unwrap();
    assert_eq!(rig.watermark(), Some(5000));
}

#[test]
fn the_first_session_has_no_gap() {
    let mut rig = rig(1000);
    let gap = rig
        .engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(gap, None);
    assert_eq!(
        rig.sessions(),
        vec![Session {
            start_ms: 1000,
            end_ms: None,
            end: None
        }]
    );
}

#[test]
fn a_clean_stop_gives_an_exact_bridge_down_gap() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(2000);
    rig.engine
        .end_session(&rig.registry, SessionEnd::Clean)
        .unwrap();
    rig.set(5000);
    let gap = rig
        .engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let expected = Gap {
        start_ms: 2000,
        end_ms: 5000,
        start_exact: true,
        cause: GapCause::BridgeDown,
        count: 1,
    };
    assert_eq!(gap, Some(expected));
    assert!(!expected.is_uncertain());
    assert_eq!(rig.gaps(), vec![expected]);
    assert_eq!(
        rig.sessions(),
        vec![
            Session {
                start_ms: 1000,
                end_ms: Some(2000),
                end: Some(SessionEnd::Clean)
            },
            Session {
                start_ms: 5000,
                end_ms: None,
                end: None
            }
        ]
    );
}

#[test]
fn history_disabled_in_between_gives_a_disabled_gap() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(2000);
    rig.engine
        .end_session(&rig.registry, SessionEnd::Disabled)
        .unwrap();
    rig.set(9000);
    let gap = rig
        .engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(
        gap.map(|g| (g.cause, g.start_ms, g.start_exact)),
        Some((GapCause::Disabled, 2000, true))
    );
}

#[test]
fn a_crash_gives_an_uncertain_gap_from_the_watermark_not_from_any_end() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(3000);
    rig.engine.heartbeat(&rig.registry).unwrap();
    // The process dies here: no end_session, nothing flushed. A new process:
    let mut next = engine_at(&rig.location);
    let registry = PinRegistry::with_clock(Arc::new(|| 9000));
    let gap = next.begin_session(&registry, &rig.limits).unwrap().unwrap();
    assert_eq!(
        (gap.start_ms, gap.end_ms, gap.start_exact, gap.cause),
        (3000, 9000, false, GapCause::BridgeDown)
    );
    assert!(gap.is_uncertain());
    let sessions = read_sessions(next.connection()).unwrap();
    assert_eq!(sessions[0].end, Some(SessionEnd::Unclean));
    assert_eq!(sessions[0].end_ms, Some(3000));
}

#[test]
fn a_crash_is_recovered_once() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(3000);
    rig.engine.heartbeat(&rig.registry).unwrap();
    let mut next = engine_at(&rig.location);
    let at_9000 = PinRegistry::with_clock(Arc::new(|| 9000));
    next.begin_session(&at_9000, &rig.limits).unwrap();
    // It crashes again at once; the first crash must not produce a second gap.
    let mut third = engine_at(&rig.location);
    let at_9500 = PinRegistry::with_clock(Arc::new(|| 9500));
    let gap = third.begin_session(&at_9500, &rig.limits).unwrap().unwrap();
    assert_eq!(gap.start_ms, 9000, "from the second session's own start");
    // The two crash gaps touch, so they are one span of two.
    assert_eq!(
        read_gaps(third.connection()).unwrap(),
        vec![Gap {
            start_ms: 3000,
            end_ms: 9500,
            start_exact: false,
            cause: GapCause::BridgeDown,
            count: 2
        }]
    );
}

#[test]
fn a_stop_with_an_uncommitted_item_is_unclean_and_its_gap_starts_at_the_pin() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(2000);
    let _pending = rig.registry.accept().unwrap();
    rig.set(4000);
    rig.engine
        .end_session(&rig.registry, SessionEnd::Clean)
        .unwrap();
    let sessions = rig.sessions();
    assert_eq!(sessions[0].end, Some(SessionEnd::Unclean));
    assert_eq!(
        sessions[0].end_ms,
        Some(2000),
        "not 4000: that is not backed"
    );
    rig.set(8000);
    let fresh = PinRegistry::with_clock(Arc::new(|| 8000));
    let gap = rig
        .engine
        .begin_session(&fresh, &rig.limits)
        .unwrap()
        .unwrap();
    assert_eq!((gap.start_ms, gap.start_exact), (2000, false));
}

#[test]
fn a_session_started_with_a_pin_held_does_not_raise_the_watermark_past_it() {
    let mut rig = rig(1000);
    let _prompt = rig.registry.accept().unwrap();
    rig.set(5000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(rig.watermark(), Some(1000));
}

#[test]
fn commit_pinned_rejects_more_items_than_rows_per_txn_before_any_transaction() {
    let mut rig = rig(1000);
    let max = rig.engine.budget().rows_per_txn as usize;
    let batch = PinnedBatch {
        items: (0..=max as i64).map(|i| item(1000 + i)).collect(),
        pins: vec![rig.registry.accept().unwrap()],
    };
    let err = rig.engine.commit_pinned(&batch, &rig.registry).unwrap_err();
    assert!(matches!(
        err,
        DurableError::Write(WriteError::BatchTooLarge { .. })
    ));
    assert_eq!(rig.count("events"), 0);
    assert_eq!(rig.registry.len(), 1, "the pin is untouched");
    assert_eq!(rig.watermark(), None);
}

#[test]
fn a_duplicate_or_released_pin_is_refused_before_anything_commits() {
    let mut rig = rig(1000);
    let gap = Gap {
        start_ms: 1000,
        end_ms: 1200,
        start_exact: true,
        cause: GapCause::QueueOverflow,
        count: 4,
    };
    let live = rig.registry.accept().unwrap();
    let stale = rig.registry.accept().unwrap();
    rig.registry.release(&stale).unwrap();
    for pins in [vec![&live, &live], vec![&live, &stale]] {
        let err = rig
            .engine
            .commit_gap(&gap, &pins, &rig.registry, &rig.limits)
            .unwrap_err();
        assert!(matches!(err, DurableError::Pin(_)), "{err:?}");
        assert!(rig.gaps().is_empty(), "nothing committed");
        assert_eq!(rig.watermark(), None);
        assert_eq!(rig.registry.len(), 1, "no pin released");
    }
}

#[test]
fn a_flush_refused_before_the_transaction_neither_clears_nor_double_counts() {
    let mut rig = rig(1000);
    let mut counter = LossCounter::new(GapCause::QueueOverflow);
    counter.record(&rig.registry, 3).unwrap();
    rig.set(2000);
    let other = PinRegistry::new();
    let err = rig
        .engine
        .flush_counter(&mut counter, &other, &rig.limits)
        .unwrap_err();
    assert!(matches!(err, DurableError::Pin(_)));
    assert!(rig.gaps().is_empty());
    assert_eq!(counter.pending_count(), 3, "still pending, not cleared");
    rig.engine
        .flush_counter(&mut counter, &rig.registry, &rig.limits)
        .unwrap();
    assert_eq!(rig.gaps().iter().map(|g| g.count).sum::<u64>(), 3);
    assert_eq!(counter.pending_count(), 0);
}
