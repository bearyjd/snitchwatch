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
    /// Items lost, at least, among the rows given. Every part of it is a
    /// lower bound. A row that a page window clips is counted in full on
    /// every page it overlaps, so these totals describe one window and must
    /// not be added across pages.
    pub lost_at_least: u64,
    pub gap_rows: usize,
    /// Upstream-loss rows that touch an outage on one side only (the other
    /// half was merged away or lies outside the window). Each is counted one
    /// lower than its row says, so the total stays a lower bound; this says
    /// how many rows were treated so.
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

fn touches(a: &Gap, b: &Gap) -> bool {
    a.start_ms <= b.end_ms && a.end_ms >= b.start_ms
}

fn root(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Rows of outages and the upstream-loss rows they touch, connected: the
/// ledger merges rows of one cause that overlap, so after a burst of outages
/// one upstream row can hold halves of several, and which half belongs to
/// which outage is no longer known. Each component has at least one of each.
fn components(gaps: &[Gap]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..gaps.len()).collect();
    let is = |i: usize, cause| gaps[i].cause == cause;
    for d in (0..gaps.len()).filter(|&i| is(i, GapCause::DaemonDisconnected)) {
        for u in (0..gaps.len()).filter(|&i| is(i, GapCause::UpstreamLoss)) {
            if touches(&gaps[d], &gaps[u]) {
                let (rd, ru) = (root(&mut parent, d), root(&mut parent, u));
                parent[rd] = ru;
            }
        }
    }
    let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..gaps.len() {
        if is(i, GapCause::DaemonDisconnected) || is(i, GapCause::UpstreamLoss) {
            by_root.entry(root(&mut parent, i)).or_default().push(i);
        }
    }
    by_root
        .into_values()
        .filter(|rows| {
            rows.iter().any(|&i| is(i, GapCause::DaemonDisconnected))
                && rows.iter().any(|&i| is(i, GapCause::UpstreamLoss))
        })
        .collect()
}

/// `(before, after)` of every clean bracketing pair: one outage of count 1,
/// with one upstream-loss row ending inside it and another starting inside
/// it. Anything else (a row alone, merged rows) is not claimed as a pair.
fn find_pairs(gaps: &[Gap]) -> Vec<(usize, usize)> {
    let mut used = vec![false; gaps.len()];
    let mut pairs = Vec::new();
    for down in gaps
        .iter()
        .filter(|g| g.cause == GapCause::DaemonDisconnected)
    {
        let inside = |t: i64| t >= down.start_ms && t <= down.end_ms;
        let mut take = |pick: &dyn Fn(&Gap) -> bool| -> Vec<usize> {
            let picked: Vec<usize> = (0..gaps.len())
                .filter(|&i| !used[i] && gaps[i].cause == GapCause::UpstreamLoss && pick(&gaps[i]))
                .collect();
            picked.iter().for_each(|&i| used[i] = true);
            picked
        };
        let before = take(&|g| inside(g.end_ms));
        let after = take(&|g| inside(g.start_ms));
        if let ([b], [a], 1) = (&before[..], &after[..], down.count) {
            pairs.push((*b, *a));
        }
    }
    pairs
}

/// Which rows are half of a bracketing pair: shown as "at least 1 on each
/// side", never summed into a claim.
pub fn pair_flags(gaps: &[Gap]) -> Vec<bool> {
    let mut flags = vec![false; gaps.len()];
    for (before, after) in find_pairs(gaps) {
        flags[before] = true;
        flags[after] = true;
    }
    flags
}

fn sum(gaps: &[Gap], rows: &[usize], cause: GapCause) -> u64 {
    rows.iter()
        .filter(|&&i| gaps[i].cause == cause)
        .fold(0u64, |sum, &i| sum.saturating_add(gaps[i].count.max(1)))
}

/// What a component's rows prove at least. Each outage wrote one pre-outage
/// assertion of 1 and a post-outage row of at least 1: the rows sum to its
/// loss, or to 2 for a loss of 1, so the rows hold at most one assertion per
/// outage that may not be a loss. The outage rows' counts are how many
/// outages there were; subtracting them all (never more than the rows) keeps
/// the bound a lower bound whichever outage's half a merged row holds.
fn component_lower_bound(gaps: &[Gap], rows: &[usize]) -> u64 {
    let upstream = sum(gaps, rows, GapCause::UpstreamLoss);
    let outages = sum(gaps, rows, GapCause::DaemonDisconnected);
    upstream.saturating_sub(outages.min(upstream)).max(1)
}

/// Rows that can stand where an outage row used to be: a failed write, an L1
/// collapse into an `uncertain` span, or an eviction removed it.
fn hides_outages(cause: GapCause) -> bool {
    matches!(
        cause,
        GapCause::Uncertain | GapCause::WriteFailed | GapCause::Evicted
    )
}

/// An upstream-loss row that overlaps or abuts a row that can hide an outage:
/// it may be half of a pair whose outage row is gone, and then both halves
/// would count in full (a loss of 1 reads as 2). A row that the page window
/// clips is treated alike: the outage row between its halves may lie outside.
fn near_hidden(gaps: &[Gap], i: usize, window: (i64, i64)) -> bool {
    let clipped = gaps[i].start_ms < window.0 || gaps[i].end_ms > window.1;
    gaps[i].cause == GapCause::UpstreamLoss
        && (clipped
            || gaps
                .iter()
                .any(|g| hides_outages(g.cause) && touches(g, &gaps[i])))
}

/// What a pool of upstream rows whose outages may be missing proves at least.
/// Every outage's rows sum to its loss, or to 2 for a loss of 1, so rows
/// summing to `r` come from a loss of at least `ceil(r / 2)`. The pool is
/// bounded once: bounding groups separately could count the two halves of one
/// outage twice.
fn pool_lower_bound(rows: u64) -> u64 {
    rows.div_ceil(2)
}

pub fn summarize(gaps: &[Gap]) -> Summary {
    summarize_window(gaps, i64::MIN, i64::MAX)
}

/// `summarize` for rows loaded for the window `[lo, hi]`.
pub fn summarize_window(gaps: &[Gap], lo: i64, hi: i64) -> Summary {
    let window = (lo, hi);
    let flags = pair_flags(gaps);
    let mut in_comp = vec![false; gaps.len()];
    let mut grouped = 0u64;
    let mut unclaimed = 0usize;
    let mut pool = 0u64;
    for rows in &components(gaps) {
        let hidden = rows.iter().any(|&i| near_hidden(gaps, i, window));
        if hidden {
            pool = pool.saturating_add(sum(gaps, rows, GapCause::UpstreamLoss));
        } else {
            grouped = grouped.saturating_add(component_lower_bound(gaps, rows));
        }
        for &i in rows {
            in_comp[i] = true;
            unclaimed += usize::from(gaps[i].cause == GapCause::UpstreamLoss && !flags[i]);
        }
    }
    let mut singles = 0u64;
    for (i, g) in gaps.iter().enumerate().filter(|(i, _)| !in_comp[*i]) {
        if near_hidden(gaps, i, window) {
            pool = pool.saturating_add(g.count.max(1));
            unclaimed += 1;
        } else if counts_items(g.cause) {
            singles = singles.saturating_add(g.count);
        }
    }
    Summary {
        lost_at_least: singles
            .saturating_add(grouped)
            .saturating_add(pool_lower_bound(pool)),
        gap_rows: gaps.len(),
        may_overcount_by_pairs: unclaimed,
    }
}

#[cfg(test)]
#[path = "gap_counts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "gap_counts_merge_tests.rs"]
mod merge_tests;
