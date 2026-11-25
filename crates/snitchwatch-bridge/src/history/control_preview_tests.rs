//! PR 10: preview counts and changes nothing.

use std::fs;

use super::control::{Limits, Refused};
use super::control_support::count;
use super::control_tests::{feed, gate_rig, wait_for};
use super::reader_registry::MAX_READERS;

fn rows(rig: &super::control_tests::GateRig) -> i64 {
    count(&rig.location, "SELECT COUNT(*) FROM events")
}

#[test]
fn a_preview_changes_nothing() {
    let rig = gate_rig(Limits::default());
    feed(&rig, 3);
    wait_for("rows", || rows(&rig) == 3);
    let capture = rig.gate.slot().get().unwrap();
    let db = fs::read(rig.location.db_path()).unwrap();
    let settings = fs::read(rig.location.settings_path()).unwrap();
    let generation = rig.gate.generation().current();
    let queued = capture.queued_for_test();
    let preview = rig.gate.preview().unwrap();
    assert_eq!(
        (preview.count, preview.from_ms, preview.to_ms),
        (3, Some(20), Some(22))
    );
    assert!(preview.approx_bytes > 0);
    assert_eq!(fs::read(rig.location.db_path()).unwrap(), db);
    assert_eq!(fs::read(rig.location.settings_path()).unwrap(), settings);
    assert_eq!(rig.gate.generation().current(), generation);
    assert_eq!(capture.queued_for_test(), queued);
    assert!(std::sync::Arc::ptr_eq(
        &capture,
        &rig.gate.slot().get().unwrap()
    ));
    assert_eq!(rig.gate.readers().in_use(), 0, "the slot is returned");
}

#[test]
fn at_most_four_tokens_are_live() {
    let rig = gate_rig(Limits::default());
    let first = rig.gate.preview().unwrap().token;
    for _ in 0..4 {
        rig.gate.preview().unwrap();
    }
    assert_eq!(rig.gate.clear(first), Err(Refused::StaleToken));
}

#[test]
fn a_preview_needs_a_reader_slot_and_an_open_registry() {
    let rig = gate_rig(Limits::default());
    let held: Vec<_> = (0..MAX_READERS)
        .map(|_| rig.gate.readers().try_acquire().unwrap())
        .collect();
    assert_eq!(rig.gate.preview().err(), Some(Refused::Busy));
    drop(held);
    let closed = rig
        .gate
        .readers()
        .quiesce(std::time::Duration::from_secs(1));
    assert_eq!(rig.gate.preview().err(), Some(Refused::InProgress));
    drop(closed);
    assert!(rig.gate.preview().is_ok());
}

#[test]
fn a_preview_counts_across_every_scan_window() {
    let rig = gate_rig(Limits::default());
    let capture = rig.gate.slot().get().unwrap();
    capture.quiesce_writer_for_test();
    let db = rusqlite::Connection::open(rig.location.db_path()).unwrap();
    db.execute_batch(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 45001) \
         INSERT INTO events (ts_ms, action, answered_by, repeat_count) \
         SELECT i, 'allow', 'rule', 1 + (i % 3) FROM n;",
    )
    .unwrap();
    drop(db);
    let preview = rig.gate.preview().unwrap();
    let expected = count(&rig.location, "SELECT SUM(repeat_count) FROM events");
    assert_eq!(preview.count as i64, expected);
    assert_eq!((preview.from_ms, preview.to_ms), (Some(1), Some(45_001)));
}
