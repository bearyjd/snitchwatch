//! #432 / #419: when an outage's upstream-loss rows and its daemon row are
//! due in one flush they commit together; every other tally commits alone.

use std::sync::{Arc, Mutex};

use super::super::durable::tests::{rig, Rig};
use super::super::gap_counts::summarize;
use super::super::ledger::{read_gaps, Gap, GapCause};
use super::repair_tests::{
    assert_covered, assert_watermark_held, beat_ok, fail_nth, losses, ping, rows,
    rows_after_a_crash,
};
use super::*;

/// A session whose first ping, at 1000, holds the guard pin.
fn pinged() -> (Rig, Mutex<Losses>) {
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
    (rig, losses)
}

/// The daemon is away from `down` to `up`.
fn outage_between(rig: &Rig, losses: &Mutex<Losses>, down: i64, up: i64) {
    rig.set(down);
    locked(losses).daemon_down(&rig.registry);
    rig.set(up);
    locked(losses).daemon_up(up);
}

/// A ping at `at` reporting `missing` events lost after `since`.
fn ping_reports(rig: &Rig, losses: &Mutex<Losses>, missing: u64, since: i64, at: i64) {
    rig.set(at);
    locked(losses).upstream_loss(&rig.registry, ping(missing, Some(since), at));
}

/// An outage across which `missing` events were lost: the first ping after the
/// daemon came back reports them, which becomes a pre-outage row, a
/// post-outage row (for one event, count 1 each: #419) and the daemon row.
fn lost_across_an_outage(missing: u64) -> (Rig, Mutex<Losses>) {
    let (rig, losses) = pinged();
    outage_between(&rig, &losses, 2000, 4000);
    ping_reports(&rig, &losses, missing, 1000, 4500);
    rig.set(5000);
    (rig, losses)
}

/// Fails every insert of a daemon row on the writer's connection, whichever
/// transaction it is in.
fn refuse_daemon_rows(rig: &Rig) {
    rig.engine
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_daemon BEFORE INSERT ON main.gaps
             WHEN NEW.cause = 'daemon-disconnected'
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .unwrap();
}

fn accept_daemon_rows(rig: &Rig) {
    rig.engine
        .connection()
        .execute_batch("DROP TRIGGER refuse_daemon")
        .unwrap();
}

fn flush(rig: &mut Rig, losses: &Mutex<Losses>) -> Result<(), DurableError> {
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, losses, false)
}

fn upstream_count(rows: &[Gap]) -> u64 {
    rows.iter()
        .filter(|g| g.cause == GapCause::UpstreamLoss)
        .map(|g| g.count)
        .sum()
}

/// Everything is written: the one pin left is the guard of the ping at `at`.
fn assert_only_the_guard_is_pinned(rig: &Rig, at: i64) {
    assert_eq!(
        (rig.registry.len(), rig.registry.oldest()),
        (1, Some(at)),
        "only the last ping's guard stays pinned"
    );
}

// ---- a whole outage commits together ---------------------------------------

/// A reader on its own connection, run before every commit: how many gap
/// rows each read between the writer's commits could have seen.
fn row_counts_between_commits(rig: &mut Rig) -> Arc<Mutex<Vec<usize>>> {
    let counts = Arc::new(Mutex::new(Vec::new()));
    let (seen, db) = (counts.clone(), rig.location.db_path());
    rig.engine.fault_before_commit_for_test(Box::new(move || {
        let conn = rusqlite::Connection::open(&db).unwrap();
        seen.lock().unwrap().push(read_gaps(&conn).unwrap().len());
        None
    }));
    counts
}

#[test]
fn one_lost_event_across_an_outage_never_reads_as_two_between_commits() {
    let (mut rig, losses) = lost_across_an_outage(1);
    let counts = row_counts_between_commits(&mut rig);
    flush(&mut rig, &losses).unwrap();
    assert_eq!(rows(&rig).len(), 3, "{:?}", rows(&rig));
    assert_eq!(summarize(&rows(&rig)).lost_at_least, 1, "n = 1 reads 1");
    let counts = counts.lock().unwrap();
    assert!(!counts.is_empty(), "the reader never ran");
    assert!(
        counts.iter().all(|n| *n == 0 || *n == 3),
        "a read between two commits saw part of the outage: {counts:?}"
    );
}

#[test]
fn a_failed_outage_commit_writes_none_of_its_rows_and_releases_no_pin() {
    // Whichever commit of the flush fails, the outage is never left in part.
    for nth in 0..3 {
        let (mut rig, losses) = lost_across_an_outage(1);
        let pins = rig.registry.len();
        rig.engine.fault_before_commit_for_test(fail_nth(nth));
        let done = flush(&mut rig, &losses);
        let written = rows(&rig);
        assert!(
            written.is_empty() || written.len() == 3,
            "nth={nth}: all three or none: {written:?}"
        );
        assert_eq!(done.is_err(), written.is_empty(), "nth={nth}");
        if nth == 0 {
            assert!(done.is_err(), "the first commit is the outage's");
            assert_eq!(rig.registry.len(), pins, "no pin released");
        }
        assert_watermark_held(&rig, "after the flush");
        // Every tally came back: the retry writes the whole outage.
        flush(&mut rig, &losses).unwrap();
        let all = rows(&rig);
        assert_eq!(all.len(), 3, "nth={nth}: {all:?}");
        assert_eq!(summarize(&all).lost_at_least, 1, "nth={nth}");
        assert_only_the_guard_is_pinned(&rig, 4500);
    }
}

#[test]
fn a_failed_outage_group_commit_keeps_the_watermark_before_the_lost_stretch() {
    let (mut rig, losses) = lost_across_an_outage(3);
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
    assert!(rows(&rig).is_empty(), "{:?}", rows(&rig));
    assert_watermark_held(&rig, "after the failed group");
    let after = rows_after_a_crash(&rig, 6000);
    assert_covered(
        &after,
        1000,
        4500,
        "nothing from the last ping to the first",
    );
}

// ---- every other tally commits alone ----------------------------------------

#[test]
fn an_upstream_loss_with_no_halves_survives_a_failed_daemon_row() {
    let (mut rig, losses) = pinged();
    ping_reports(&rig, &losses, 2, 1000, 1500);
    outage_between(&rig, &losses, 2000, 4000);
    // The first ping after the outage lost nothing more: no halves.
    ping_reports(&rig, &losses, 0, 1500, 4500);
    rig.set(5000);
    refuse_daemon_rows(&rig);
    assert!(flush(&mut rig, &losses).is_err());
    assert_eq!(upstream_count(&rows(&rig)), 2, "{:?}", rows(&rig));
    assert_watermark_held(&rig, "after the failed daemon row");
    assert_eq!(upstream_count(&rows_after_a_crash(&rig, 6000)), 2);
    // Without the crash the retry writes the daemon row and frees its pin.
    accept_daemon_rows(&rig);
    flush(&mut rig, &losses).unwrap();
    assert_only_the_guard_is_pinned(&rig, 4500);
}

/// Halves already written leave nothing to group: a later ordinary loss does
/// not wait on the daemon row of the outage that is still unwritten.
#[test]
fn an_ordinary_loss_after_written_halves_is_not_grouped_with_the_daemon_row() {
    let (mut rig, losses) = lost_across_an_outage(1);
    locked(&losses).daemon_down(&rig.registry);
    flush(&mut rig, &losses).unwrap();
    assert_eq!(upstream_count(&rows(&rig)), 2, "the halves, written alone");
    rig.set(5500);
    locked(&losses).daemon_up(5500);
    ping_reports(&rig, &losses, 0, 4500, 5600);
    ping_reports(&rig, &losses, 2, 5600, 5800);
    rig.set(6000);
    refuse_daemon_rows(&rig);
    assert!(flush(&mut rig, &losses).is_err());
    assert_eq!(upstream_count(&rows(&rig)), 4, "{:?}", rows(&rig));
}

#[test]
fn a_crash_during_a_second_outage_keeps_the_first_outages_counts() {
    let (mut rig, losses) = lost_across_an_outage(3);
    locked(&losses).daemon_down(&rig.registry);
    rig.set(5500);
    beat_ok(&mut rig, &losses, &mut ClockWatch::new(), 0);
    assert_watermark_held(&rig, "during the second outage");
    let after = rows_after_a_crash(&rig, 6000);
    assert_eq!(upstream_count(&after), 3, "{after:?}");
    assert_covered(&after, 1000, 6000, "from the last ping to the restart");
}

/// The halves of an outage whose daemon row already committed are not grouped
/// with a later outage's row, so its failure does not take them along.
#[test]
fn the_halves_of_a_written_outage_do_not_wait_on_the_next_outages_row() {
    let (mut rig, losses) = pinged();
    outage_between(&rig, &losses, 2000, 4000);
    flush(&mut rig, &losses).unwrap();
    assert_eq!(rows(&rig).len(), 1, "the daemon row alone");
    ping_reports(&rig, &losses, 3, 1000, 4500);
    outage_between(&rig, &losses, 5000, 5500);
    rig.set(6000);
    refuse_daemon_rows(&rig);
    assert!(flush(&mut rig, &losses).is_err());
    assert_eq!(upstream_count(&rows(&rig)), 3, "{:?}", rows(&rig));
}

/// The daemon row can commit alone, before the ping that reports the loss has
/// tallied it; a half that then fails to write must still hold the watermark
/// before its stretch.
#[test]
fn a_failed_half_after_the_daemon_row_committed_alone_holds_the_watermark() {
    let (mut rig, losses) = pinged();
    outage_between(&rig, &losses, 2000, 4000);
    rig.set(4200);
    flush(&mut rig, &losses).unwrap();
    assert_eq!(rows(&rig).len(), 1, "the daemon row alone");
    ping_reports(&rig, &losses, 3, 1000, 4500);
    rig.set(5000);
    rig.engine.fault_before_commit_for_test(fail_nth(0));
    assert!(flush(&mut rig, &losses).is_err());
    assert_eq!(rows(&rig).len(), 2, "the other half committed alone");
    assert_watermark_held(&rig, "after the failed half");
    let mut all = rows(&rig);
    all.extend(rows_after_a_crash(&rig, 6000));
    assert_covered(&all, 1000, 4500, "nothing from the last ping to the first");
}

/// A known, transient overstatement: when the daemon drops again before an
/// outage's rows were flushed, its halves are written without a daemon row
/// (that tally is now the open outage), and one lost event reads as two until
/// the second outage closes and writes it.
#[test]
fn a_second_outage_before_the_flush_overstates_only_until_it_closes() {
    let (mut rig, losses) = lost_across_an_outage(1);
    locked(&losses).daemon_down(&rig.registry);
    flush(&mut rig, &losses).unwrap();
    assert_eq!(rows(&rig).len(), 2, "{:?}", rows(&rig));
    assert_eq!(summarize(&rows(&rig)).lost_at_least, 2, "transient");
    rig.set(6000);
    locked(&losses).daemon_up(6000);
    flush(&mut rig, &losses).unwrap();
    assert_eq!(rows(&rig).len(), 3, "{:?}", rows(&rig));
    assert_eq!(summarize(&rows(&rig)).lost_at_least, 1);
    assert_only_the_guard_is_pinned(&rig, 4500);
}
