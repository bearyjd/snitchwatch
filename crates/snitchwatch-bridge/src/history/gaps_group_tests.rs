//! #432 / #419: an outage's upstream-loss rows and its daemon row commit
//! together, and what waits for what when they cannot.

use std::sync::{Arc, Mutex};

use super::super::durable::tests::{rig, Rig};
use super::super::gap_counts::summarize;
use super::super::ledger::{read_gaps, GapCause};
use super::super::session::read_watermark;
use super::repair_tests::{
    assert_covered, assert_watermark_held, beat_ok, fail_nth, losses, ping, rows,
    rows_after_a_crash,
};
use super::*;

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
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    one_event_lost_across_an_outage(&rig, &losses);
    let counts = row_counts_between_commits(&mut rig);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
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
        let mut rig = rig(1000);
        rig.engine
            .begin_session(&rig.registry, &rig.limits)
            .unwrap();
        let losses = losses();
        one_event_lost_across_an_outage(&rig, &losses);
        let pins = rig.registry.len();
        rig.engine.fault_before_commit_for_test(fail_nth(nth));
        let done = flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false);
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
        flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
        let all = rows(&rig);
        assert_eq!(all.len(), 3, "nth={nth}: {all:?}");
        assert_eq!(summarize(&all).lost_at_least, 1, "nth={nth}");
        assert!(rig.registry.len() < pins, "nth={nth}: pins released");
    }
}

#[test]
fn a_daemon_that_drops_again_before_the_flush_holds_the_upstream_rows_with_its_tally() {
    let mut rig = rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = losses();
    one_event_lost_across_an_outage(&rig, &losses);
    // The daemon drops again before the first outage's rows were flushed: the
    // pending daemon tally is now an open outage and has no end.
    locked(&losses).daemon_down(&rig.registry);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    assert!(
        rows(&rig).is_empty(),
        "both upstream halves wait for the daemon row: {:?}",
        rows(&rig)
    );
    assert_watermark_held(&rig, "while the halves are held");
    rig.set(6000);
    locked(&losses).daemon_up(6000);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    let all = rows(&rig);
    assert_eq!(
        all.iter()
            .filter(|g| g.cause == GapCause::UpstreamLoss)
            .count(),
        2,
        "{all:?}"
    );
}

/// The daemon row can commit alone, before the ping that reports the loss
/// has tallied it; the group commit that then fails must still hold the
/// watermark before the lost stretch.
#[test]
fn a_failed_group_after_the_daemon_row_committed_alone_holds_the_watermark() {
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
    rig.set(4200);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    assert_eq!(rows(&rig).len(), 1, "the daemon row alone");
    rig.set(4500);
    locked(&losses).upstream_loss(&rig.registry, ping(3, Some(1000), 4500));
    rig.set(5000);
    rig.engine.fault_before_commit_for_test(fail_nth(0));
    let failed = flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false);
    assert!(failed.is_err());
    assert_eq!(rows(&rig).len(), 1, "the group wrote nothing");
    assert_watermark_held(&rig, "after the failed group");
    let mut all = rows(&rig);
    all.extend(rows_after_a_crash(&rig, 6000));
    assert_covered(&all, 1000, 4500, "nothing from the last ping to the first");
}

/// #432 round 2: only the halves of an outage whose daemon row is pending wait
/// for it. A loss no outage touched is written while the daemon is away.
#[test]
fn an_upstream_loss_unrelated_to_an_open_outage_is_written_during_it() {
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
    locked(&losses).upstream_loss(&rig.registry, ping(1, Some(1000), 2000));
    rig.set(3000);
    locked(&losses).daemon_down(&rig.registry);
    rig.set(4000);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    let all = rows(&rig);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!((all[0].cause, all[0].count), (GapCause::UpstreamLoss, 1));
    assert_eq!(
        read_watermark(rig.engine.connection()).unwrap(),
        Some(2000),
        "the loss no longer holds the watermark"
    );
}

/// The halves of an outage whose daemon row already committed are not held by
/// a later outage's tally.
#[test]
fn the_halves_of_a_written_outage_are_not_held_by_the_next_one() {
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
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    assert_eq!(rows(&rig).len(), 1, "the daemon row alone");
    rig.set(4500);
    locked(&losses).upstream_loss(&rig.registry, ping(3, Some(1000), 4500));
    rig.set(5000);
    locked(&losses).daemon_down(&rig.registry);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    let all = rows(&rig);
    assert_eq!(
        all.iter()
            .filter(|g| g.cause == GapCause::UpstreamLoss)
            .count(),
        2,
        "{all:?}"
    );
    assert_eq!(all.len(), 3, "{all:?}");
}

/// #432, the accepted limit: a loss no outage touched, still in the `upstream`
/// tally when an outage half joins it, waits with that half if the daemon
/// drops again before the flush. A crash then keeps the span covered (the
/// bridge-down gap starts at the held watermark) but not the count. Without
/// the second drop the count is written before the crash.
#[test]
fn a_crash_with_a_held_mixed_tally_covers_the_span_and_loses_only_its_count() {
    for second_drop in [false, true] {
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
        rig.set(1500);
        locked(&losses).upstream_loss(&rig.registry, ping(2, Some(1000), 1500));
        rig.set(2000);
        locked(&losses).daemon_down(&rig.registry);
        rig.set(4000);
        locked(&losses).daemon_up(4000);
        rig.set(4500);
        locked(&losses).upstream_loss(&rig.registry, ping(1, Some(1500), 4500));
        rig.set(5000);
        if second_drop {
            locked(&losses).daemon_down(&rig.registry);
        }
        rig.set(5500);
        beat_ok(&mut rig, &losses, &mut clock, 0);
        assert_watermark_held(&rig, "before the crash");
        let after = rows_after_a_crash(&rig, 6000);
        assert_covered(&after, 1000, 6000, &format!("second_drop={second_drop}"));
        let counted: u64 = after
            .iter()
            .filter(|g| g.cause == GapCause::UpstreamLoss)
            .map(|g| g.count)
            .sum();
        // Written: the unrelated 2 and the outage's pair of 1 + 1 (#419).
        let expected = if second_drop { 0 } else { 4 };
        assert_eq!(counted, expected, "second_drop={second_drop}: {after:?}");
    }
}

#[test]
fn a_failed_outage_group_commit_keeps_the_watermark_before_the_lost_stretch() {
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
