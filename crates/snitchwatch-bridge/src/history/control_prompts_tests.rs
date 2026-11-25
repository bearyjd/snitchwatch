//! PR 10 / #420: a prompt accepted before the capture stops is never
//! silently dropped. When it settles too late it is covered by one gap and
//! its pin is released with that gap, never by the discard alone (W1).

use std::sync::atomic::Ordering;

use rusqlite::Connection as Db;

use super::control_support::{conn, count, rig, Rig};
use super::guard::DecisionOutcome;

fn gaps(rig: &Rig) -> Vec<(String, i64, i64)> {
    let db = Db::open(rig.location.db_path()).unwrap();
    let mut stmt = db
        .prepare("SELECT cause, count, start_ms FROM gaps ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_prompt_settled_after_stop_is_covered_by_a_write_failed_gap() {
    let rig = rig();
    let guard = rig.capture.begin_decision(&conn("/a", "h")).unwrap();
    rig.clock.store(10_500, Ordering::SeqCst);
    let Rig {
        capture,
        registry,
        location,
        root: _root,
        ..
    } = rig;
    capture.shutdown();
    guard.settle(DecisionOutcome::GuiLost);
    assert_eq!(count(&location, "SELECT COUNT(*) FROM events"), 0);
    let db = Db::open(location.db_path()).unwrap();
    let rows: Vec<(String, i64, i64)> = db
        .prepare("SELECT cause, count, start_ms FROM gaps")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, vec![("write-failed".to_owned(), 1, 10_000)]);
    assert_eq!(registry.len(), 0, "the pin left with its gap, not before");
}

#[test]
fn prompts_pending_at_disable_are_one_disabled_gap_from_the_earliest() {
    let rig = rig();
    let first = rig.capture.begin_decision(&conn("/a", "h")).unwrap();
    rig.clock.store(10_400, Ordering::SeqCst);
    let second = rig.capture.begin_decision(&conn("/b", "h")).unwrap();
    rig.clock.store(10_900, Ordering::SeqCst);
    let probe = gaps_probe(&rig);
    let Rig {
        capture, registry, ..
    } = rig;
    capture.disable();
    first.settle(DecisionOutcome::GuiLost);
    second.settle(DecisionOutcome::GuiLost);
    assert_eq!(probe(), vec![("disabled".to_owned(), 2, 10_000)]);
    assert_eq!(registry.len(), 0);
}

fn gaps_probe(rig: &Rig) -> impl Fn() -> Vec<(String, i64, i64)> {
    let path = rig.location.db_path();
    move || {
        let db = Db::open(&path).unwrap();
        let mut stmt = db
            .prepare("SELECT cause, count, start_ms FROM gaps ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

#[test]
fn a_prompt_settled_before_stop_is_recorded_and_leaves_no_gap() {
    let rig = rig();
    let guard = rig.capture.begin_decision(&conn("/a", "h")).unwrap();
    guard.settle(DecisionOutcome::GuiLost);
    assert_eq!(gaps(&rig), vec![]);
    let Rig {
        capture, location, ..
    } = rig;
    capture.shutdown();
    assert_eq!(count(&location, "SELECT COUNT(*) FROM events"), 1);
}

#[test]
fn a_mismatched_settle_keeps_its_pin_pending_for_the_gap() {
    use std::sync::Arc;

    use super::capture::Capture;
    use super::control_support::{config, consent};
    use super::pins::PinRegistry;
    use super::record::{Action, AnsweredBy, EventRecord};

    let registry = Arc::new(PinRegistry::new());
    let capture = Capture::new_unstarted_for_test(registry.clone(), config());
    let shared = capture.shared_for_test();
    let pin = registry.accept().unwrap();
    let key = shared.pending_add_for_test(pin, 1);
    // No consent is active: this settle is stale.
    let record = EventRecord::new(1, Action::PendingUnanswered, AnsweredBy::Unknown);
    shared.settle(
        record,
        &consent(),
        shared.generation_for_test().current(),
        key,
        DecisionOutcome::GuiLost,
    );
    assert_eq!(registry.len(), 1, "a discard alone must not release a pin");
    assert_eq!(shared.pending_len_for_test(), 1);
}
