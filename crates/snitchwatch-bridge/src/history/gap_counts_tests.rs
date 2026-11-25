use super::*;
use crate::history::ledger::GapCause;
use proptest::prelude::*;

fn row(start: i64, end: i64, cause: GapCause, count: u64) -> Gap {
    Gap {
        start_ms: start,
        end_ms: end,
        start_exact: true,
        cause,
        count,
    }
}

/// What PR 7 writes after an outage that lost `n` events upstream: the stretch
/// from the last ping to the down edge, the outage, and daemon-up to the ping
/// (gaps.rs: both halves have count >= 1).
fn outage(n: u64) -> Vec<Gap> {
    let (c1, c2) = if n >= 2 { (n / 2, n - n / 2) } else { (1, 1) };
    vec![
        row(100, 200, GapCause::UpstreamLoss, c1),
        row(200, 300, GapCause::DaemonDisconnected, 1),
        row(300, 400, GapCause::UpstreamLoss, c2),
    ]
}

#[test]
fn the_single_loss_outage_pair_is_one_not_two() {
    let gaps = outage(1);
    assert_eq!(summarize(&gaps).lost_at_least, 1, "rows say 1 + 1");
}

#[test]
fn a_larger_outage_pair_is_at_least_n_minus_one() {
    assert_eq!(summarize(&outage(5)).lost_at_least, 4);
    assert_eq!(summarize(&outage(2)).lost_at_least, 1);
}

#[test]
fn both_halves_of_the_pair_are_marked_and_the_outage_is_not() {
    assert_eq!(pair_flags(&outage(5)), vec![true, false, true]);
}

#[test]
fn rows_that_do_not_touch_the_outage_are_not_paired() {
    let gaps = vec![
        row(100, 190, GapCause::UpstreamLoss, 3),
        row(200, 300, GapCause::DaemonDisconnected, 1),
        row(310, 400, GapCause::UpstreamLoss, 3),
    ];
    assert_eq!(pair_flags(&gaps), vec![false, false, false]);
    assert_eq!(summarize(&gaps).lost_at_least, 6);
    assert_eq!(summarize(&gaps).may_overcount_by_pairs, 0);
}

#[test]
fn a_pair_broken_apart_by_a_merge_says_it_may_overcount() {
    // The post-outage half was merged away: only the first half is adjacent.
    let gaps = vec![
        row(100, 200, GapCause::UpstreamLoss, 1),
        row(200, 300, GapCause::DaemonDisconnected, 1),
    ];
    let summary = summarize(&gaps);
    assert_eq!(pair_flags(&gaps), vec![false, false]);
    assert_eq!(summary.lost_at_least, 1);
    assert_eq!(summary.may_overcount_by_pairs, 1);
    // The other half alone, likewise.
    let gaps = vec![
        row(200, 300, GapCause::DaemonDisconnected, 1),
        row(300, 400, GapCause::UpstreamLoss, 1),
    ];
    assert_eq!(summarize(&gaps).may_overcount_by_pairs, 1);
}

#[test]
fn a_surviving_half_of_a_broken_pair_never_counts_more_than_it_can_prove() {
    // The other half was merged away or fell outside the window: the row's
    // count may include a loss that happened on the missing side.
    for gaps in [
        vec![
            row(100, 200, GapCause::UpstreamLoss, 5),
            row(200, 300, GapCause::DaemonDisconnected, 1),
        ],
        vec![
            row(200, 300, GapCause::DaemonDisconnected, 1),
            row(300, 400, GapCause::UpstreamLoss, 5),
        ],
    ] {
        assert_eq!(summarize(&gaps).lost_at_least, 4);
    }
    let one = vec![
        row(100, 200, GapCause::UpstreamLoss, 1),
        row(200, 300, GapCause::DaemonDisconnected, 1),
    ];
    assert_eq!(summarize(&one).lost_at_least, 1, "never below one");
}

#[test]
fn one_upstream_row_is_never_the_half_of_two_pairs() {
    let gaps = vec![
        row(100, 200, GapCause::UpstreamLoss, 2),
        row(200, 300, GapCause::DaemonDisconnected, 1),
        row(300, 400, GapCause::UpstreamLoss, 2),
        row(400, 500, GapCause::DaemonDisconnected, 1),
        row(500, 600, GapCause::UpstreamLoss, 2),
    ];
    // Chain: the middle row sits between two outages. It is used once.
    let flags = pair_flags(&gaps);
    assert_eq!(flags, vec![true, false, true, false, false]);
    assert_eq!(
        summarize(&gaps).lost_at_least,
        3 + 1,
        "the unpaired half counts n-1"
    );
    assert_eq!(summarize(&gaps).may_overcount_by_pairs, 1);
}

#[test]
fn only_causes_that_count_items_add_to_the_total() {
    let gaps = vec![
        row(0, 10, GapCause::BridgeDown, 4),
        row(10, 20, GapCause::DaemonDisconnected, 4),
        row(20, 30, GapCause::Disabled, 4),
        row(30, 40, GapCause::ClockJump, 4),
        row(40, 50, GapCause::Uncertain, 4),
        row(50, 60, GapCause::QueueOverflow, 3),
        row(60, 70, GapCause::AdmissionDrop, 5),
        row(70, 80, GapCause::WriteFailed, 7),
        row(80, 90, GapCause::Evicted, 11),
        row(90, 99, GapCause::UpstreamLoss, 13),
    ];
    let summary = summarize(&gaps);
    assert_eq!(summary.lost_at_least, 3 + 5 + 7 + 11 + 13);
    assert_eq!(summary.gap_rows, 10);
}

#[test]
fn the_total_saturates_instead_of_wrapping() {
    let gaps = vec![
        row(0, 1, GapCause::QueueOverflow, u64::MAX),
        row(1, 2, GapCause::QueueOverflow, 5),
    ];
    assert_eq!(summarize(&gaps).lost_at_least, u64::MAX);
}

proptest! {
    /// Never over-reports: for any true loss `n` spread over an outage the
    /// way the writer does it, the displayed lower bound is at most `n`, and
    /// at least 1.
    #[test]
    fn the_outage_total_never_exceeds_the_truth(n in 1u64..10_000, others in 0u64..50) {
        let mut gaps = outage(n);
        gaps.push(row(1000, 1100, GapCause::QueueOverflow, others.max(1)));
        let shown = summarize(&gaps).lost_at_least - others.max(1);
        prop_assert!(shown <= n, "shown {shown} for truth {n}");
        prop_assert!(shown >= 1);
    }
}

#[test]
fn a_row_cannot_be_both_halves_of_one_outage() {
    // A zero-length outage and one zero-length upstream row at the same
    // instant: the row touches the outage on both sides, but it is one row.
    let gaps = vec![
        row(200, 200, GapCause::UpstreamLoss, 1),
        row(200, 200, GapCause::DaemonDisconnected, 1),
    ];
    assert_eq!(pair_flags(&gaps), vec![false, false]);
    assert_eq!(summarize(&gaps).lost_at_least, 1);
    assert_eq!(summarize(&gaps).may_overcount_by_pairs, 1);
}
