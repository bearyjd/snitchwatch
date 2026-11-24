//! PR 8 repair 3: pre-epoch events leave a valid marker, evicted markers of
//! different shapes are never merged over time that still has events, the
//! scans never use file-backed temp storage, and the pressure pass gives way
//! to shutdown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::record::Action;
use super::retention_support::*;
use super::writer::TxnKind;

fn pre_epoch(rig: &Rig, negatives: i64, survivor: Option<i64>) -> Vec<i64> {
    let ts: Vec<i64> = (0..negatives).map(|i| -1_000 + i).collect();
    fill(
        rig,
        ts.iter()
            .copied()
            .chain(survivor)
            .map(|t| event(t, "/usr/bin/a", Action::Allow)),
    );
    ts
}

#[test]
fn a_chunk_of_pre_epoch_events_leaves_a_valid_marker() {
    let mut rig = rig();
    let rows = rig.engine.budget().rows_per_txn as i64;
    let ts = pre_epoch(&rig, rows + 1, None);
    rig.engine.evict_chunk().expect("the chunk must commit");
    assert_eq!(count(&rig, "events"), 1);
    assert_removed_are_covered(&rig, &ts[..rows as usize]);
}

#[test]
fn a_marker_covers_pre_epoch_events_when_a_later_survivor_exists() {
    let mut rig = rig();
    let rows = rig.engine.budget().rows_per_txn as i64;
    let ts = pre_epoch(&rig, rows, Some(500));
    rig.engine.evict_chunk().unwrap();
    assert_eq!(remaining(&rig).len(), 1);
    assert_removed_are_covered(&rig, &ts);
}

#[test]
fn the_engine_scans_in_memory_not_in_a_temp_file() {
    let rig = rig();
    let store: i64 = rig
        .engine
        .connection()
        .query_row("PRAGMA temp_store", [], |r| r.get(0))
        .unwrap();
    assert_eq!(store, 2, "temp_store must be MEMORY (2)");
}

#[test]
fn the_pressure_pass_stops_after_the_chunk_in_which_shutdown_arrived() {
    let mut rig = rig();
    fill_rotating(&rig, 12, 3, 0);
    let stop = Arc::new(AtomicBool::new(false));
    let (flag, seen) = (stop.clone(), rig.reports.clone());
    rig.engine.set_observer(Box::new(move |r| {
        seen.lock().unwrap().push(*r);
        if r.kind == TxnKind::Evict {
            flag.store(true, Ordering::SeqCst);
        }
    }));
    let flag = stop.clone();
    rig.engine
        .set_stop(Box::new(move || flag.load(Ordering::SeqCst)));
    let before = count(&rig, "events");
    let _ = rig.engine.make_room().unwrap();
    let unvacuumed: i64 = rig
        .engine
        .connection()
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))
        .unwrap();
    assert!(unvacuumed > 0, "the vacuum ran on after shutdown");
    let evicts = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == TxnKind::Evict)
        .count();
    assert_eq!(evicts, 1, "the pass ran on after shutdown was asked for");
    let marked: u64 = evicted(&rig).iter().map(|g| g.count).sum();
    assert_eq!(marked as i64, before - count(&rig, "events"));
    assert!(marked > 0);
}

/// A flooder over its share (tier 2) plus rotating rows over high water.
fn flooded(rig: &Rig) {
    let mut ts = 1_000_000;
    while bytes_of(rig, "/opt/flood") <= 2 * share_bytes(rig) {
        fill(
            rig,
            (0..64).map(|_| {
                ts += 1;
                event(ts, "/opt/flood", Action::Allow)
            }),
        );
    }
    fill_rotating(rig, 12, 3, 0);
}

#[test]
fn tier_two_also_stops_after_the_chunk_in_which_shutdown_arrived() {
    let mut rig = rig();
    flooded(&rig);
    let stop = Arc::new(AtomicBool::new(false));
    let (flag, seen) = (stop.clone(), rig.reports.clone());
    rig.engine.set_observer(Box::new(move |r| {
        seen.lock().unwrap().push(*r);
        flag.store(true, Ordering::SeqCst);
    }));
    rig.engine
        .set_stop(Box::new(move || stop.load(Ordering::SeqCst)));
    let _ = rig.engine.make_room().unwrap();
    let evicts = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == TxnKind::Evict)
        .count();
    assert_eq!(evicts, 1, "tier 2 ran on after shutdown was asked for");
}
