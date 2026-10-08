//! How often each daemon rule decided a connection (P2.6 Part 1; plan
//! `docs/superpowers/plans/2026-10-08-rule-insights.md`).
//!
//! opensnitchd reports only global `rule_hits` / `rule_misses`. Per-rule
//! counts are derived from `Statistics.events` in its pings, and those are
//! **incomplete by construction**: each ping carries at most
//! `Stats.MaxEvents` matched connections (the oldest are dropped), nothing is
//! reported while no bridge is connected, and a `nolog` rule produces no
//! event at all. So every count here is a lower bound, and this state records
//! *when it may have missed events* ([`RuleHits::last_gap_unix_ms`]) instead
//! of pretending to be complete. A gap is noted when:
//!
//! - a ping's batch is within one of the daemon's cap (a missed connection at
//!   the cap drops an event without adding one, so a batch that lost events
//!   can end at `max_events - 1`);
//! - the daemon's `uptime` drops (it restarted; the counts are **kept**, only
//!   the events in between are unknown);
//! - the counts were restored from a file (the bridge was down meanwhile);
//! - a bound below was hit, or a rule name was too long to keep.
//!
//! **Which map an event lands in.** An event whose rule the bridge's rule
//! cache knows counts in the main map. Anything else waits in a bounded side
//! map: the bridge's own once-reply names (never in the daemon's list), a
//! rule the cache doesn't know yet, or any event while the cache is
//! `Unknown` (between a daemon stream closing and the next snapshot). A
//! committed snapshot moves the side entries it names into the main map and
//! forgets the rest, and drops main-map names it lacks. A confirmed
//! `DELETE_RULE` drops that name. **Nothing prunes on `withdraw`**: it runs on
//! every daemon reconnect and would wipe every count.
//!
//! Counts restored from the saved file wait the same way (`restored`) until
//! the first committed snapshot says which of them still exist. Until then
//! they are kept out of the wire message but included in what is saved, so
//! a bridge that runs for hours without a daemon doesn't lose its history.

use std::collections::BTreeMap;

use snitchwatch_proto::protocol::Event;

use crate::cache::rules::MAX_SNAPSHOT_RULES;
use crate::ws_messages::RuleHitWire;

/// `Stats.MaxEvents` when the daemon's config doesn't say (`stats.go`).
pub const DEFAULT_MAX_EVENTS: usize = 150;
/// Most names waiting for a snapshot at once.
pub const SIDE_MAP_MAX: usize = 1_000;
/// Most rules counted (and saved) at once: the largest snapshot staged.
pub const MAX_TRACKED_RULES: usize = MAX_SNAPSHOT_RULES;
/// A longer rule name (or one with control characters) is not counted, and
/// counts as a gap.
pub const MAX_HIT_NAME_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Stat {
    count: u64,
    last_hit_unix_ms: i64,
}

impl Stat {
    fn merge(&mut self, other: Stat) {
        self.count = self.count.saturating_add(other.count);
        self.last_hit_unix_ms = self.last_hit_unix_ms.max(other.last_hit_unix_ms);
    }
}

/// What is saved between bridge runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub since_unix_ms: i64,
    pub last_gap_unix_ms: Option<i64>,
    pub hits: Vec<RuleHitWire>,
}

/// The counts. Pure: times and the rule cache's membership are passed in.
#[derive(Debug, Default)]
pub struct RuleHits {
    main: BTreeMap<String, Stat>,
    side: BTreeMap<String, Stat>,
    restored: BTreeMap<String, Stat>,
    since_unix_ms: Option<i64>,
    last_gap_unix_ms: Option<i64>,
    last_uptime: Option<u64>,
    revision: u64,
}

impl RuleHits {
    /// Counts the events of one ping that carried statistics. `known` says
    /// whether the rule cache is synced and has that rule. Returns whether
    /// anything a client sees changed.
    pub fn record(
        &mut self,
        events: &[Event],
        uptime: u64,
        max_events: usize,
        now_ms: i64,
        known: impl Fn(&str) -> bool,
    ) -> bool {
        let before = self.revision;
        if self.since_unix_ms.is_none() {
            self.since_unix_ms = Some(now_ms);
            self.touch();
        }
        if self.last_uptime.is_some_and(|previous| uptime < previous) {
            self.note_gap(now_ms);
        }
        self.last_uptime = Some(uptime);
        let max_events = if max_events == 0 {
            DEFAULT_MAX_EVENTS
        } else {
            max_events
        };
        // The raw length: the daemon's cap counts events without a rule too.
        if !events.is_empty() && events.len() + 1 >= max_events {
            self.note_gap(now_ms);
        }
        for event in events {
            let Some(rule) = event.rule.as_ref().filter(|rule| !rule.name.is_empty()) else {
                continue;
            };
            let at = if event.unixnano > 0 {
                event.unixnano / 1_000_000
            } else {
                now_ms
            };
            self.count(&rule.name, at, now_ms, known(&rule.name));
        }
        self.revision != before
    }

    fn count(&mut self, name: &str, at_ms: i64, now_ms: i64, known: bool) {
        if !keepable_name(name) {
            self.note_gap(now_ms);
            return;
        }
        let hit = Stat {
            count: 1,
            last_hit_unix_ms: at_ms,
        };
        if known {
            if !self.main.contains_key(name) && self.main.len() >= MAX_TRACKED_RULES {
                self.note_gap(now_ms);
                return;
            }
            let waiting = self.side.remove(name);
            let stat = self.main.entry(name.to_string()).or_default();
            if let Some(waiting) = waiting {
                stat.merge(waiting);
            }
            stat.merge(hit);
        } else {
            if !self.side.contains_key(name) && self.side.len() >= SIDE_MAP_MAX {
                self.note_gap(now_ms);
                return;
            }
            self.side.entry(name.to_string()).or_default().merge(hit);
        }
        self.touch();
    }

    /// A new snapshot was committed: `known` is its membership. Counts for
    /// names it lacks are dropped; waiting and restored counts for names it
    /// has move to the main map. Call only on a commit, never on `withdraw`.
    pub fn adopt_snapshot(&mut self, now_ms: i64, known: impl Fn(&str) -> bool) {
        let before = self.main.len();
        self.main.retain(|name, _| known(name));
        let mut changed = self.main.len() != before;
        let waiting = std::mem::take(&mut self.side);
        let restored = std::mem::take(&mut self.restored);
        for (name, stat) in waiting.into_iter().chain(restored) {
            if !known(&name) {
                continue;
            }
            if !self.main.contains_key(&name) && self.main.len() >= MAX_TRACKED_RULES {
                self.note_gap(now_ms);
                continue;
            }
            self.main.entry(name).or_default().merge(stat);
            changed = true;
        }
        if changed {
            self.touch();
        }
    }

    /// A confirmed `DELETE_RULE`.
    pub fn forget(&mut self, name: &str) {
        // Every map, not the first that has it.
        let in_main = self.main.remove(name).is_some();
        let in_side = self.side.remove(name).is_some();
        let in_restored = self.restored.remove(name).is_some();
        if in_main || in_side || in_restored {
            self.touch();
        }
    }

    /// Loads what an earlier run saved. The counts wait for the first
    /// snapshot; the start time is kept; the bridge was down since, so
    /// events may be missing.
    pub fn restore(&mut self, saved: Saved, now_ms: i64) {
        self.since_unix_ms = Some(saved.since_unix_ms);
        self.last_gap_unix_ms = saved.last_gap_unix_ms;
        for hit in saved.hits.into_iter().take(MAX_TRACKED_RULES) {
            if !keepable_name(&hit.name) {
                continue;
            }
            self.restored.insert(
                hit.name,
                Stat {
                    count: hit.count,
                    last_hit_unix_ms: hit.last_hit_unix_ms,
                },
            );
        }
        self.note_gap(now_ms);
        self.touch();
    }

    /// What to save, or `None` while counting hasn't started. The main map
    /// plus the counts restored but not yet checked against a snapshot, up
    /// to [`MAX_TRACKED_RULES`] (the main map first).
    pub fn to_saved(&self) -> Option<Saved> {
        let since_unix_ms = self.since_unix_ms?;
        let restored = self
            .restored
            .iter()
            .filter(|(name, _)| !self.main.contains_key(*name));
        let hits = self
            .main
            .iter()
            .chain(restored)
            .take(MAX_TRACKED_RULES)
            .map(|(name, stat)| wire(name, stat))
            .collect();
        Some(Saved {
            since_unix_ms,
            last_gap_unix_ms: self.last_gap_unix_ms,
            hits,
        })
    }

    /// The counts clients see, in name order.
    pub fn wire_hits(&self) -> Vec<RuleHitWire> {
        self.main
            .iter()
            .map(|(name, stat)| wire(name, stat))
            .collect()
    }

    pub fn since_unix_ms(&self) -> Option<i64> {
        self.since_unix_ms
    }

    /// Whether events may be missing from the counts. Never clears.
    pub fn is_lossy(&self) -> bool {
        self.last_gap_unix_ms.is_some()
    }

    pub fn last_gap_unix_ms(&self) -> Option<i64> {
        self.last_gap_unix_ms
    }

    /// Changes whenever [`Self::wire_hits`] or the header fields change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn note_gap(&mut self, now_ms: i64) {
        if self.last_gap_unix_ms != Some(now_ms) {
            self.last_gap_unix_ms = Some(now_ms);
            self.touch();
        }
    }

    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Whether a rule name can be counted and saved: not too long, and no
/// control characters, so the saved file's size is bounded (a control
/// character escapes to six bytes in JSON).
pub fn keepable_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_HIT_NAME_BYTES && !name.chars().any(char::is_control)
}

fn wire(name: &str, stat: &Stat) -> RuleHitWire {
    RuleHitWire {
        name: name.to_string(),
        count: stat.count,
        last_hit_unix_ms: stat.last_hit_unix_ms,
    }
}

#[cfg(test)]
#[path = "rule_hits/tests.rs"]
mod tests;
