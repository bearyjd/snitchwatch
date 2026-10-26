//! Per-group verdict counts for "last hour" and "today" (phase 0b).
//!
//! Every group keeps sparse one-minute buckets for the hour and one total for
//! the local day, both updated as rows are inserted, updated and removed, so
//! the coarse tick never rescans rows: it only re-reads at most 60 buckets per
//! header on screen ([`GroupTree::refresh_window_figures`]). Buckets that fell
//! out of the hour are skipped on read and pruned on the next write; the day
//! total starts again on the first read or write after local midnight.
//!
//! A row is placed by its own `started_at_ms`, so a row standing for collapsed
//! repeats counts once, at its first start. "By default" means the firewall's
//! default action decided the row: `decided_by_default`, or a decided row with
//! no matched rule (a user's answer always records its once-off rule).

use std::collections::VecDeque;

use snitchwatch_bridge::ws_messages::ConnectionRow;

use super::super::row_store::{matched_rule_name, Verdict};
use super::{GroupCounts, GroupTree, VisibleEntry};

const MINUTE_MS: i64 = 60_000;
/// Minutes in the "last hour" window, the current one included.
const HOUR_MINUTES: i64 = 60;

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
    /// Sorted by minute; only minutes inside the hour are written.
    minutes: VecDeque<(i64, WindowCounts)>,
    /// The day these counts belong to (its local midnight) and the counts.
    day: (i64, WindowCounts),
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

    fn tally(&mut self, facts: &RowFacts, clock: &Clock, up: bool) {
        let floor = minute_of(clock.now_ms) - (HOUR_MINUTES - 1);
        while self.minutes.front().is_some_and(|(m, _)| *m < floor) {
            self.minutes.pop_front();
        }
        let minute = minute_of(facts.started_at_ms);
        if minute >= floor {
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
        if self.day.0 != clock.today_start_ms {
            self.day = (clock.today_start_ms, WindowCounts::default());
        }
        if facts.started_at_ms >= clock.today_start_ms {
            self.day.1.bump(facts, up);
        }
    }

    /// Both windows as of `clock`.
    pub(super) fn figures(&self, clock: &Clock) -> WindowFigures {
        let floor = minute_of(clock.now_ms) - (HOUR_MINUTES - 1);
        let last_hour = self
            .minutes
            .iter()
            .filter(|(m, _)| *m >= floor)
            .fold(WindowCounts::default(), |acc, (_, c)| acc.plus(*c));
        let today = if self.day.0 == clock.today_start_ms {
            self.day.1
        } else {
            WindowCounts::default()
        };
        WindowFigures { last_hour, today }
    }

    /// The header's counts with both windows filled in as of `clock`.
    pub(super) fn snapshot(&self, clock: &Clock) -> GroupCounts {
        GroupCounts {
            windows: self.figures(clock),
            ..self.counts
        }
    }
}

impl GroupTree {
    /// Set the time later writes and projections read the windows against.
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
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
