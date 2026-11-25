//! Connection history PR 9: gap counts are lower bounds, and a lower bound
//! must never be summed into an over-claim (#419).
//!
//! After a daemon outage the first ping's upstream loss of `n` is written as
//! two `upstream-loss` rows with `count >= 1` each: last ping to the down
//! edge, and daemon-up to the ping, with the `daemon-disconnected` row
//! between. For `n = 1` the rows sum to 2; for `n >= 2` they sum to `n`, but
//! the pre-outage row asserts a loss that may not have happened. Read time
//! pairs the rows by adjacency and counts `max(1, c1 + c2 - 1)` for the pair,
//! which never exceeds the truth. No schema change.
//!
//! Only causes that count lost items add to the total. A `bridge-down`,
//! `daemon-disconnected`, `disabled` or `clock-jump` row counts occurrences
//! of a stretch, and an `uncertain` row counts restarts, not events.

use super::ledger::{Gap, GapCause};

/// What a range of gap rows adds up to, never as one exact number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// Items lost, at least. Every part of it is a lower bound.
    pub lost_at_least: u64,
    pub gap_rows: usize,
    /// Outage rows that touch an upstream-loss row on one side only (the
    /// other half was merged away): their total can err high by one each.
    pub may_overcount_by_pairs: usize,
}

fn counts_items(cause: GapCause) -> bool {
    matches!(
        cause,
        GapCause::UpstreamLoss
            | GapCause::QueueOverflow
            | GapCause::AdmissionDrop
            | GapCause::WriteFailed
            | GapCause::Evicted
    )
}

/// `(before, outage, after)` row indexes of every bracketing pair, and the
/// outage rows that only have one side.
struct Pairs {
    pairs: Vec<(usize, usize, usize)>,
    broken: usize,
}

fn find_pairs(gaps: &[Gap]) -> Pairs {
    let mut used = vec![false; gaps.len()];
    let mut found = Pairs {
        pairs: Vec::new(),
        broken: 0,
    };
    let upstream = |i: usize| gaps[i].cause == GapCause::UpstreamLoss;
    for (d, down) in gaps.iter().enumerate() {
        if down.cause != GapCause::DaemonDisconnected {
            continue;
        }
        let free = |used: &[bool], pick: &dyn Fn(usize) -> bool| {
            (0..gaps.len()).find(|&i| i != d && upstream(i) && !used[i] && pick(i))
        };
        let before = free(&used, &|i| gaps[i].end_ms == down.start_ms);
        let after = free(&used, &|i| gaps[i].start_ms == down.end_ms && Some(i) != before);
        match (before, after) {
            (Some(b), Some(a)) => {
                used[b] = true;
                used[a] = true;
                found.pairs.push((b, d, a));
            }
            (None, None) => {}
            _ => found.broken += 1,
        }
    }
    found
}

/// Which rows are half of a bracketing pair: shown as "at least 1 on each
/// side", never summed into a claim.
pub fn pair_flags(gaps: &[Gap]) -> Vec<bool> {
    let mut flags = vec![false; gaps.len()];
    for (before, _, after) in find_pairs(gaps).pairs {
        flags[before] = true;
        flags[after] = true;
    }
    flags
}

pub fn summarize(gaps: &[Gap]) -> Summary {
    let found = find_pairs(gaps);
    let flags = pair_flags(gaps);
    let singles = gaps
        .iter()
        .zip(&flags)
        .filter(|(g, paired)| !**paired && counts_items(g.cause))
        .fold(0u64, |sum, (g, _)| sum.saturating_add(g.count));
    let paired = found.pairs.iter().fold(0u64, |sum, (b, _, a)| {
        let both = gaps[*b].count.saturating_add(gaps[*a].count);
        sum.saturating_add(both.saturating_sub(1).max(1))
    });
    Summary {
        lost_at_least: singles.saturating_add(paired),
        gap_rows: gaps.len(),
        may_overcount_by_pairs: found.broken,
    }
}

#[cfg(test)]
#[path = "gap_counts_tests.rs"]
mod tests;
