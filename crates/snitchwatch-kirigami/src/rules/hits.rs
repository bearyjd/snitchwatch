//! What the Rules tab shows about how often each rule decided a connection:
//! the bridge's last `RuleHits` message (P2.6 Part 1; the bridge side is
//! `snitchwatch_bridge::cache::rule_hits`). Pure and Qt-free.
//!
//! The counts are Snitchwatch's own tally of the daemon's per-ping events,
//! so they can be low: events are dropped when a ping carries more than the
//! daemon's cap, nothing is reported while no bridge is connected, and a
//! `nolog` rule reports nothing at all. The view therefore never shows a
//! number it can't stand behind:
//!
//! - nothing is shown before a `RuleHits` arrives from the live session (an
//!   older bridge never sends one), or before counting has started;
//! - a `nolog` rule is "not counted", never `0`, and so is a rule whose name
//!   the bridge can't count ([`keepable_name`]);
//! - the summary says since when the counts run, that they are approximate,
//!   whether events may be missing and since when, and whether they survive a
//!   restart of the bridge.

use std::collections::HashMap;

use serde::Serialize;
use snitchwatch_bridge::cache::rule_hits::keepable_name;
use snitchwatch_bridge::ws_messages::{ServerMessage, StorageStatus};

use crate::rules::row_store::Rule;

/// What a rule row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowHits {
    /// No count to show (yet).
    Unavailable,
    /// The daemon reports no events for this rule, so any number would
    /// mislead.
    NotCounted,
    /// The bridge doesn't count a rule with this name (over 256 bytes, or
    /// with a control character), so `0` would be false.
    NameNotCounted,
    Counted {
        count: u64,
        last_hit_unix_ms: i64,
    },
}

impl RowHits {
    /// Why a row has no count, when that's worth saying.
    pub fn note(&self) -> &'static str {
        match self {
            Self::NotCounted => "Not counted: this rule doesn't log",
            Self::NameNotCounted => {
                "Not counted: this rule's name is too long or has control characters"
            }
            _ => "",
        }
    }

    /// The count as the model's `real` role: a QML `int` stops at
    /// 2 147 483 647. Exact up to 2^53.
    pub fn count_for_model(&self) -> f64 {
        match self {
            Self::Counted { count, .. } => *count as f64,
            _ => 0.0,
        }
    }
}

#[derive(Debug)]
struct Received {
    since_unix_ms: Option<i64>,
    lossy: bool,
    last_gap_unix_ms: Option<i64>,
    storage: StorageStatus,
    counts: HashMap<String, (u64, i64)>,
}

/// The summary the page shows above the list, as JSON for QML.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Info<'a> {
    /// A `RuleHits` arrived from the live session.
    available: bool,
    /// The bridge has begun counting (it has seen the daemon's statistics).
    counting: bool,
    since_ms: i64,
    lossy: bool,
    last_gap_ms: i64,
    persistent: bool,
    storage_reason: &'a str,
}

/// How the counts were made: what a "no hits" badge needs to know to be
/// honest (`rules::insights::hit_badge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counting {
    pub since_unix_ms: i64,
    /// The counts are saved across restarts of the bridge.
    pub persistent: bool,
    /// Hits may be missing from the counts.
    pub lossy: bool,
    /// When the bridge last noticed a gap, if it said.
    pub last_gap_unix_ms: Option<i64>,
}

#[derive(Debug, Default)]
pub struct RuleHitsView {
    session: Option<u64>,
    received: Option<Received>,
}

impl RuleHitsView {
    /// Applies a `RuleHits`; returns whether `msg` was one.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        let ServerMessage::RuleHits {
            since_unix_ms,
            lossy,
            last_gap_unix_ms,
            storage,
            hits,
        } = msg
        else {
            return false;
        };
        self.received = Some(Received {
            since_unix_ms: *since_unix_ms,
            lossy: *lossy,
            last_gap_unix_ms: *last_gap_unix_ms,
            storage: storage.clone(),
            counts: hits
                .iter()
                .map(|hit| (hit.name.clone(), (hit.count, hit.last_hit_unix_ms)))
                .collect(),
        });
        true
    }

    /// A message arrived from bridge session `connection_id`. Counts from an
    /// earlier session are forgotten, so a bridge that doesn't send
    /// `RuleHits` is never shown the last one's. Returns whether anything
    /// was forgotten.
    pub fn note_session(&mut self, connection_id: u64) -> bool {
        if self.session == Some(connection_id) {
            return false;
        }
        self.session = Some(connection_id);
        self.received.take().is_some()
    }

    pub fn for_rule(&self, rule: &Rule) -> RowHits {
        let Some(received) = self.received.as_ref().filter(|r| r.since_unix_ms.is_some()) else {
            return RowHits::Unavailable;
        };
        if rule.nolog {
            return RowHits::NotCounted;
        }
        if !keepable_name(&rule.name) {
            return RowHits::NameNotCounted;
        }
        let (count, last_hit_unix_ms) = received.counts.get(&rule.name).copied().unwrap_or((0, 0));
        RowHits::Counted {
            count,
            last_hit_unix_ms,
        }
    }

    /// How the counts were made, once counting has started.
    pub fn counting(&self) -> Option<Counting> {
        let received = self.received.as_ref()?;
        Some(Counting {
            since_unix_ms: received.since_unix_ms?,
            persistent: received.storage.persistent,
            lossy: received.lossy,
            last_gap_unix_ms: received.last_gap_unix_ms,
        })
    }

    pub fn info_json(&self) -> String {
        let info = match &self.received {
            None => Info {
                available: false,
                counting: false,
                since_ms: 0,
                lossy: false,
                last_gap_ms: 0,
                persistent: false,
                storage_reason: "",
            },
            Some(r) => Info {
                available: true,
                counting: r.since_unix_ms.is_some(),
                since_ms: r.since_unix_ms.unwrap_or(0),
                lossy: r.lossy,
                last_gap_ms: r.last_gap_unix_ms.unwrap_or(0),
                persistent: r.storage.persistent,
                storage_reason: r.storage.reason.as_deref().unwrap_or_default(),
            },
        };
        serde_json::to_string(&info).unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "hits/tests.rs"]
mod tests;
