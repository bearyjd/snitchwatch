//! PR 8 repair: an age-expiry pass is bounded in time and chunks, and gives
//! way to shutdown, so a long pass cannot hold the writer past
//! `SHUTDOWN_DRAIN`.

use std::time::{Duration, Instant};

use super::record::Action;
use super::retention_support::*;

const DAY_MS: i64 = 86_400_000;
const NOW: i64 = 400 * DAY_MS;

fn old_rows(rig: &mut Rig, n: i64) -> Vec<i64> {
    let stamps: Vec<i64> = (0..n).map(|i| 1_000 + i).collect();
    // Rows of about a page each, so deleting them frees pages to vacuum.
    put(rig, stamps.iter().map(|ts| plain(big(*ts))).collect());
    put(rig, vec![plain(event(NOW, "/p", Action::Allow))]);
    stamps
}

#[test]
fn a_pass_told_to_stop_leaves_the_rest_for_the_next_one_and_still_covers_what_it_took() {
    let mut rig = rig();
    let stamps = old_rows(&mut rig, 1500);
    let mut asked = 0;
    let pass = rig
        .engine
        .expire_aged_until(NOW, false, &mut || {
            asked += 1;
            asked > 2
        })
        .unwrap();
    assert!(pass.stopped, "it gave way");
    assert_eq!(pass.removed, 64, "two chunks of 32, then it stopped");
    assert_eq!(count(&rig, "events"), 1501 - 64);
    assert_removed_are_covered(&rig, &stamps[..64]);
    assert!(
        rig.reports
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.kind != super::writer::TxnKind::Vacuum),
        "a pass that gave way does not hold the writer for a vacuum"
    );
    // The next pass carries on from there.
    let next = rig.engine.expire_aged(NOW, false).unwrap();
    assert!(!next.stopped && next.removed > 0);
}

#[test]
fn a_pass_told_to_stop_at_once_removes_nothing_and_does_not_vacuum() {
    let mut rig = rig();
    old_rows(&mut rig, 100);
    rig.reports.lock().unwrap().clear();
    let pass = rig
        .engine
        .expire_aged_until(NOW, false, &mut || true)
        .unwrap();
    assert!(pass.stopped);
    assert_eq!(pass.removed, 0);
    assert_eq!(count(&rig, "events"), 101);
    assert!(
        rig.reports.lock().unwrap().is_empty(),
        "no transaction at all"
    );
}

#[test]
fn the_writers_stop_condition_is_a_closed_queue_or_the_deadline() {
    use super::capture::{Capture, CaptureConfig};
    use super::pins::PinRegistry;
    use std::sync::Arc;
    let rig = rig();
    let Rig { _root, engine, .. } = rig;
    let config = CaptureConfig::for_budget(engine.budget());
    let capture = Capture::spawn(engine, Arc::new(PinRegistry::new()), config).unwrap();
    let far = Instant::now() + Duration::from_secs(3600);
    assert!(!capture.shared.expiry_stop(far)(), "open and in time");
    assert!(
        capture
            .shared
            .expiry_stop(Instant::now() - Duration::from_millis(1))(),
        "past its deadline"
    );
    capture.close_queue_for_test();
    assert!(capture.shared.expiry_stop(far)(), "the queue is closed");
    capture.shutdown();
}

fn freelist(rig: &Rig) -> i64 {
    rig.engine
        .connection()
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))
        .unwrap()
}

fn vacuums(rig: &Rig) -> usize {
    vacuums_of(&rig.reports)
}

#[test]
fn the_vacuum_after_a_pass_asks_to_stop_between_its_steps() {
    let mut rig = rig();
    old_rows(&mut rig, 1500);
    let reports = rig.reports.clone();
    // Quiet through the chunks; told to stop once one vacuum step has run.
    let mut stop = || vacuums_of(&reports) > 0;
    let pass = rig.engine.expire_aged_until(NOW, false, &mut stop).unwrap();
    assert!(pass.stopped, "the vacuum gave way");
    assert!(pass.removed > 64, "the chunks ran whole: {}", pass.removed);
    assert_eq!(vacuums(&rig), 1, "one step, then it asked");
    assert!(freelist(&rig) > 0, "pages are left for the next pass");
}

#[test]
fn a_pass_that_removes_nothing_still_returns_the_pages_an_earlier_one_left() {
    let mut rig = rig();
    old_rows(&mut rig, 1500);
    let reports = rig.reports.clone();
    let mut stop = || vacuums_of(&reports) > 0;
    rig.engine.expire_aged_until(NOW, false, &mut stop).unwrap();
    assert!(freelist(&rig) > 0);
    let before = vacuums(&rig);
    // A wall time at which nothing is old: the pass deletes nothing.
    let next = rig.engine.expire_aged(2_000, false).unwrap();
    assert_eq!(next.removed, 0);
    assert!(!next.stopped);
    assert_eq!(freelist(&rig), 0, "the leftover pages went back");
    assert!(vacuums(&rig) > before);
}

fn vacuums_of(reports: &std::sync::Mutex<Vec<super::writer::TxnReport>>) -> usize {
    reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == super::writer::TxnKind::Vacuum)
        .count()
}
