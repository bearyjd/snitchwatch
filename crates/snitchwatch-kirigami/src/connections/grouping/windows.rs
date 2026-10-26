//! Per-group verdict counts for "last hour" and "today" (phase 0b).
//!
//! Every group keeps sparse one-minute buckets for the hour and one total for
//! the local day, both updated as rows are inserted, updated and removed, so
//! the coarse tick never rescans rows: it only re-reads a header's buckets
//! ([`GroupTree::refresh_window_figures`]). Buckets that fell out of the hour
//! are skipped on read and pruned on the next write; the day total starts
//! again after local midnight.
//!
//! A start in a later minute than now (clock skew) counts in neither window
//! until its minute comes: it waits in a separate bucket list, at most
//! [`FUTURE_MINUTES`] ahead. A start further out is not bucketed at all; the
//! tree rebuilds every group's windows from its rows once one comes within
//! reach. It also rebuilds when the clock goes back, or when today's start
//! moves without the clock passing it (a time zone change), since buckets
//! already pruned can't be restored any other way. Each rebuild reads every
//! row once and is rare; steady forward time never does it.
//!
//! A row is placed by its own `started_at_ms`, so a row standing for collapsed
//! repeats counts once, at its first start. "By default" means the firewall's
//! default action decided the row: `decided_by_default`, or a decided row with
//! no matched rule (a user's answer always records its once-off rule).

use std::collections::{BTreeMap, VecDeque};

use snitchwatch_bridge::ws_messages::ConnectionRow;

use super::super::row_store::{matched_rule_name, Verdict};
use super::{GroupCounts, GroupTree, VisibleEntry};

const MINUTE_MS: i64 = 60_000;
/// Minutes in the "last hour" window, the current one included.
pub(super) const HOUR_MINUTES: i64 = 60;
/// How far ahead of now a skewed start is kept in buckets.
pub(super) const FUTURE_MINUTES: i64 = 24 * 60;

/// The time the windows are read against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clock {
    pub now_ms: i64,
    /// Local midnight that starts the current day, in Unix ms.
    pub today_start_ms: i64,
}

/// One window's figures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowCounts {
    pub allowed_by_rule: usize,
    pub allowed_by_default: usize,
    pub denied_by_rule: usize,
    pub denied_by_default: usize,
    pub blocklisted: usize,
    pub pending: usize,
    /// Decided, but by an action this GUI can't name (`Verdict::Other`).
    pub other: usize,
}

/// A header's figures for both windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowFigures {
    pub last_hour: WindowCounts,
    pub today: WindowCounts,
}

impl WindowCounts {
    fn slot(&mut self, facts: &RowFacts) -> &mut usize {
        match (facts.verdict, facts.by_default) {
            (Verdict::Allowed, false) => &mut self.allowed_by_rule,
            (Verdict::Allowed, true) => &mut self.allowed_by_default,
            (Verdict::Denied, false) => &mut self.denied_by_rule,
            (Verdict::Denied, true) => &mut self.denied_by_default,
            (Verdict::Blocklisted, _) => &mut self.blocklisted,
            (Verdict::Pending, _) => &mut self.pending,
            (Verdict::Other, _) => &mut self.other,
        }
    }

    fn bump(&mut self, facts: &RowFacts, up: bool) {
        let slot = self.slot(facts);
        *slot = if up {
            *slot + 1
        } else {
            slot.saturating_sub(1)
        };
    }

    fn plus(self, o: Self) -> Self {
        Self {
            allowed_by_rule: self.allowed_by_rule + o.allowed_by_rule,
            allowed_by_default: self.allowed_by_default + o.allowed_by_default,
            denied_by_rule: self.denied_by_rule + o.denied_by_rule,
            denied_by_default: self.denied_by_default + o.denied_by_default,
            blocklisted: self.blocklisted + o.blocklisted,
            pending: self.pending + o.pending,
            other: self.other + o.other,
        }
    }

    /// `(kind, text)` per verdict present, e.g. `("allowed", "12 allowed (3
    /// by default)")`; `kind` picks the delegate's icon, the text says it in
    /// words.
    pub fn segments(&self) -> Vec<(&'static str, String)> {
        let split = |kind: &'static str, by_rule: usize, by_default: usize| {
            let n = by_rule + by_default;
            (n > 0).then(|| match by_default {
                0 => (kind, format!("{n} {kind}")),
                d => (kind, format!("{n} {kind} ({d} by default)")),
            })
        };
        let plain = |kind: &'static str, n: usize, word: &str| {
            (n > 0).then(|| (kind, format!("{n} {word}")))
        };
        [
            split("allowed", self.allowed_by_rule, self.allowed_by_default),
            split("denied", self.denied_by_rule, self.denied_by_default),
            plain("blocklisted", self.blocklisted, "blocklisted"),
            plain("pending", self.pending, "pending"),
            plain("other", self.other, "decided another way"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The window as one plain sentence, e.g. "Last hour: 12 allowed (3 by
    /// default) · 2 denied · 1 pending".
    pub fn summary(&self, label: &str) -> String {
        let parts: Vec<String> = self.segments().into_iter().map(|(_, t)| t).collect();
        if parts.is_empty() {
            return format!("{label}: no connections");
        }
        format!("{label}: {}", parts.join(" · "))
    }

    /// [`segments`](Self::segments) as the JSON list the delegate reads.
    pub fn segments_json(&self) -> String {
        let list: Vec<serde_json::Value> = self
            .segments()
            .into_iter()
            .map(|(kind, text)| serde_json::json!({ "kind": kind, "text": text }))
            .collect();
        serde_json::Value::Array(list).to_string()
    }
}

/// What a row contributes to its groups' counts, kept so an update or a
/// removal can subtract exactly what was added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RowFacts {
    pub verdict: Verdict,
    pub by_default: bool,
    pub started_at_ms: i64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl RowFacts {
    pub(super) fn of(row: &ConnectionRow) -> Self {
        Self {
            verdict: Verdict::of(row),
            by_default: row.decided_by_default || matched_rule_name(row).is_empty(),
            started_at_ms: row.started_at_ms,
            bytes_sent: row.bytes_sent,
            bytes_received: row.bytes_received,
        }
    }
}

fn minute_of(ms: i64) -> i64 {
    ms.div_euclid(MINUTE_MS)
}

/// A group's running totals plus its window buckets.
#[derive(Debug, Default)]
pub(super) struct GroupStats {
    pub counts: GroupCounts,
    /// The clock the buckets below are up to date with.
    seen: Clock,
    /// Sorted by minute; only minutes inside the hour, up to `seen`'s.
    minutes: VecDeque<(i64, WindowCounts)>,
    /// Starts from `seen`'s today, up to `seen`'s minute.
    day: WindowCounts,
    /// Starts after `seen`'s minute, at most [`FUTURE_MINUTES`] ahead.
    future: BTreeMap<i64, WindowCounts>,
}

fn bump_bucket(buckets: &mut VecDeque<(i64, WindowCounts)>, minute: i64, c: WindowCounts) {
    match buckets.binary_search_by_key(&minute, |(m, _)| *m) {
        Ok(i) => buckets[i].1 = buckets[i].1.plus(c),
        Err(i) => buckets.insert(i, (minute, c)),
    }
}

impl GroupStats {
    pub(super) fn add(&mut self, facts: &RowFacts, clock: &Clock) {
        self.counts
            .add(facts.verdict, facts.bytes_sent, facts.bytes_received);
        self.tally(facts, clock, true);
    }

    pub(super) fn sub(&mut self, facts: &RowFacts, clock: &Clock) {
        self.counts
            .sub(facts.verdict, facts.bytes_sent, facts.bytes_received);
        self.tally(facts, clock, false);
    }

    /// Forget the windows (not the totals), as of `clock`.
    pub(super) fn reset_windows(&mut self, clock: &Clock) {
        self.seen = *clock;
        self.minutes.clear();
        self.day = WindowCounts::default();
        self.future.clear();
    }

    /// Move the buckets forward to `clock` (the tree rebuilds instead of
    /// calling this when time went back): promote future minutes that came,
    /// prune the hour, and start the day again after midnight.
    fn catch_up(&mut self, clock: &Clock) {
        if self.seen == *clock {
            return;
        }
        let now_minute = minute_of(clock.now_ms);
        let floor = now_minute - (HOUR_MINUTES - 1);
        let day_minute = minute_of(clock.today_start_ms);
        if self.seen.today_start_ms != clock.today_start_ms {
            self.day = WindowCounts::default();
        }
        while let Some(entry) = self.future.first_entry() {
            if *entry.key() > now_minute {
                break;
            }
            let (minute, c) = entry.remove_entry();
            if minute >= floor {
                bump_bucket(&mut self.minutes, minute, c);
            }
            if minute >= day_minute {
                self.day = self.day.plus(c);
            }
        }
        while self.minutes.front().is_some_and(|(m, _)| *m < floor) {
            self.minutes.pop_front();
        }
        self.seen = *clock;
    }

    pub(super) fn tally(&mut self, facts: &RowFacts, clock: &Clock, up: bool) {
        self.catch_up(clock);
        let now_minute = minute_of(clock.now_ms);
        let minute = minute_of(facts.started_at_ms);
        if minute > now_minute {
            if minute <= now_minute + FUTURE_MINUTES {
                let c = self.future.entry(minute).or_default();
                c.bump(facts, up);
                if *c == WindowCounts::default() {
                    self.future.remove(&minute);
                }
            }
            return;
        }
        if minute >= now_minute - (HOUR_MINUTES - 1) {
            match self.minutes.binary_search_by_key(&minute, |(m, _)| *m) {
                Ok(i) => {
                    self.minutes[i].1.bump(facts, up);
                    if self.minutes[i].1 == WindowCounts::default() {
                        self.minutes.remove(i);
                    }
                }
                Err(i) if up => {
                    let mut counts = WindowCounts::default();
                    counts.bump(facts, true);
                    self.minutes.insert(i, (minute, counts));
                }
                Err(_) => {}
            }
        }
        if facts.started_at_ms >= clock.today_start_ms {
            self.day.bump(facts, up);
        }
    }

    /// Both windows as of `clock`, which is never behind `seen` (see
    /// [`catch_up`](Self::catch_up)); reads only, so a tick needs no write.
    pub(super) fn figures(&self, clock: &Clock) -> WindowFigures {
        let now_minute = minute_of(clock.now_ms);
        let floor = now_minute - (HOUR_MINUTES - 1);
        let day_minute = minute_of(clock.today_start_ms);
        let came = self.future.range(..=now_minute);
        let last_hour = self
            .minutes
            .iter()
            .chain(
                came.clone()
                    .map(|(m, c)| (*m, *c))
                    .collect::<Vec<_>>()
                    .iter(),
            )
            .filter(|(m, _)| *m >= floor)
            .fold(WindowCounts::default(), |acc, (_, c)| acc.plus(*c));
        let day = if self.seen.today_start_ms == clock.today_start_ms {
            self.day
        } else {
            WindowCounts::default()
        };
        let today = came
            .filter(|(m, _)| **m >= day_minute)
            .fold(day, |acc, (_, c)| acc.plus(*c));
        WindowFigures { last_hour, today }
    }

    /// The header's counts with both windows filled in as of `clock`.
    pub(super) fn snapshot(&self, clock: &Clock) -> GroupCounts {
        GroupCounts {
            windows: self.figures(clock),
            ..self.counts
        }
    }

    #[cfg(test)]
    pub(super) fn buckets(&self) -> usize {
        self.minutes.len() + self.future.len()
    }
}

impl GroupTree {
    /// Set the time later writes and projections read the windows against.
    /// Rebuilds every group's windows from its rows when time went back,
    /// when today's start moved without the clock passing it, or when a
    /// start beyond the future buckets' reach came within it.
    pub fn set_clock(&mut self, clock: Clock) {
        let old = self.clock;
        let midnight_passed =
            clock.today_start_ms > old.today_start_ms && clock.today_start_ms > old.now_ms;
        let moved_midnight = clock.today_start_ms != old.today_start_ms && !midnight_passed;
        let far_due = self
            .far_future_minute
            .is_some_and(|m| m <= minute_of(clock.now_ms) + FUTURE_MINUTES);
        self.clock = clock;
        if clock.now_ms < old.now_ms || moved_midnight || far_due {
            self.rebuild_windows();
        }
    }

    /// Remember a start too far ahead to bucket, so [`set_clock`] rebuilds
    /// once it comes within reach.
    pub(super) fn note_far_future(&mut self, facts: &RowFacts) {
        let minute = minute_of(facts.started_at_ms);
        if minute > minute_of(self.clock.now_ms) + FUTURE_MINUTES {
            self.far_future_minute = Some(self.far_future_minute.map_or(minute, |m| m.min(minute)));
        }
    }

    /// Recount every group's windows from the rows' kept facts.
    fn rebuild_windows(&mut self) {
        let clock = self.clock;
        for pg in self.processes.values_mut() {
            pg.stats.reset_windows(&clock);
            for dg in pg.domains.values_mut() {
                dg.stats.reset_windows(&clock);
                for hg in dg.hosts.values_mut() {
                    hg.stats.reset_windows(&clock);
                }
            }
        }
        self.far_future_minute = None;
        let metas: Vec<_> = self.id_meta.values().cloned().collect();
        for meta in metas {
            self.note_far_future(&meta.facts);
            let Some(pg) = self.processes.get_mut(&meta.process_key) else {
                continue;
            };
            pg.stats.tally(&meta.facts, &clock, true);
            let Some(dg) = pg.domains.get_mut(&meta.domain_key) else {
                continue;
            };
            dg.stats.tally(&meta.facts, &clock, true);
            if let Some(hg) = dg.hosts.get_mut(&meta.host_key) {
                hg.stats.tally(&meta.facts, &clock, true);
            }
        }
    }

    /// The most buckets any group holds (test helper for the bound).
    #[cfg(test)]
    pub(super) fn max_window_buckets(&self) -> usize {
        self.processes
            .values()
            .flat_map(|p| {
                std::iter::once(p.stats.buckets()).chain(p.domains.values().flat_map(|d| {
                    std::iter::once(d.stats.buckets())
                        .chain(d.hosts.values().map(|h| h.stats.buckets()))
                }))
            })
            .max()
            .unwrap_or(0)
    }

    /// Re-read the windows of every header in `projection` as of the current
    /// clock, in place, and return the indices whose figures changed (the
    /// model's `dataChanged` set). Cost: at most 60 buckets per header shown;
    /// no row is read.
    pub fn refresh_window_figures(&self, projection: &mut [VisibleEntry]) -> Vec<usize> {
        let mut changed = Vec::new();
        for (i, entry) in projection.iter_mut().enumerate() {
            let (stats, counts) = match entry {
                VisibleEntry::ProcessHeader { key, counts, .. } => {
                    (self.processes.get(key.as_str()).map(|p| &p.stats), counts)
                }
                VisibleEntry::DomainHeader {
                    process_key,
                    key,
                    counts,
                    ..
                } => (
                    self.processes
                        .get(process_key.as_str())
                        .and_then(|p| p.domains.get(key.as_str()))
                        .map(|d| &d.stats),
                    counts,
                ),
                VisibleEntry::HostHeader {
                    process_key,
                    domain_key,
                    key,
                    counts,
                    ..
                } => (
                    self.processes
                        .get(process_key.as_str())
                        .and_then(|p| p.domains.get(domain_key.as_str()))
                        .and_then(|d| d.hosts.get(key.as_str()))
                        .map(|h| &h.stats),
                    counts,
                ),
                VisibleEntry::Row { .. } => continue,
            };
            let Some(stats) = stats else {
                continue;
            };
            let figures = stats.figures(&self.clock);
            if counts.windows != figures {
                counts.windows = figures;
                changed.push(i);
            }
        }
        changed
    }
}
