//! Who holds opensnitchd's single prompt slot, and roughly what it costs.
//! Plan: `docs/superpowers/plans/2026-10-08-prompt-slot-ux.md`, part A.
//!
//! The daemon asks one question at a time. While an `AskRule` is open, every
//! other unmatched connection usually gets the daemon's default action and
//! never reaches the bridge. The bridge can't list those connections. It can
//! say which prompt holds the slot, since when, and, from the daemon's
//! cumulative `rule_misses` counter, a lower bound on how many connections
//! were defaulted meanwhile.
//!
//! **The baseline.** The daemon pings only when it has matched events, so the
//! last reading before a hold can be minutes old (right after login, it would
//! count every Ask answered `Unavailable` before a GUI authenticated). A
//! holder's baseline is therefore the first reading *after* the hold starts,
//! and its count stays unknown until a later reading. The same happens when
//! the daemon restarts (`uptime` drops; `rule_misses` is per daemon process)
//! or a new rule snapshot is committed (a new HELLO): that reading becomes a
//! fresh baseline. Misses before the baseline go uncounted, so the figure is
//! only ever "at least N".
//!
//! **More than one holder.** The daemon's busy check is an unlocked
//! check-then-set, so two Asks can rarely be open at once. Each holder is kept
//! by row id and released on its own. The broadcast names the oldest (by hold
//! order, never by row-id string order).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::notice::{Notice, NoticeBus};
use crate::ws_messages::ServerMessage;

/// The oldest prompt holding the slot, as broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSlotHolder {
    /// The pending row's wire id.
    pub row_id: String,
    /// "process → host", with control and bidi characters removed and each
    /// part truncated, but not HTML-escaped: clients show it as plain text.
    pub what: String,
    /// When the hold started, in Unix milliseconds.
    pub since_ms: u64,
}

/// One `rule_misses` / `uptime` reading from a daemon ping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reading {
    misses: u64,
    uptime: u64,
    rules_generation: u64,
}

#[derive(Debug, Clone)]
struct Holder {
    what: String,
    since_ms: u64,
    seq: u64,
    baseline: Option<Reading>,
    last: Option<Reading>,
}

impl Holder {
    fn observe(&mut self, reading: Reading) {
        let latest = self.last.or(self.baseline);
        let reset = latest.is_some_and(|prev| {
            reading.uptime < prev.uptime || reading.rules_generation != prev.rules_generation
        });
        if self.baseline.is_none() || reset {
            self.baseline = Some(reading);
            self.last = None;
        } else {
            self.last = Some(reading);
        }
    }

    fn defaulted_at_least(&self) -> Option<u64> {
        let (baseline, last) = (self.baseline?, self.last?);
        Some(last.misses.saturating_sub(baseline.misses))
    }
}

/// The holders and their counts. Pure: times and readings are passed in.
#[derive(Debug, Default)]
pub struct PromptSlot {
    holders: HashMap<String, Holder>,
    next_seq: u64,
}

impl PromptSlot {
    pub fn hold(&mut self, row_id: &str, what: String, since_ms: u64) {
        self.next_seq += 1;
        self.holders.insert(
            row_id.to_string(),
            Holder {
                what,
                since_ms,
                seq: self.next_seq,
                baseline: None,
                last: None,
            },
        );
    }

    /// Releases `row_id` only. `Some(count)` when it was held, with its own
    /// count; `None` when it wasn't.
    pub fn release(&mut self, row_id: &str) -> Option<Option<u64>> {
        self.holders
            .remove(row_id)
            .map(|holder| holder.defaulted_at_least())
    }

    /// A daemon ping's counters, and the current rule-snapshot generation.
    pub fn observe(&mut self, rule_misses: u64, uptime: u64, rules_generation: u64) {
        let reading = Reading {
            misses: rule_misses,
            uptime,
            rules_generation,
        };
        for holder in self.holders.values_mut() {
            holder.observe(reading);
        }
    }

    /// The `PromptSlot` message for the current state.
    pub fn message(&self) -> ServerMessage {
        let oldest = self.oldest();
        ServerMessage::PromptSlot {
            holder: oldest.map(|(row_id, h)| PromptSlotHolder {
                row_id: row_id.clone(),
                what: h.what.clone(),
                since_ms: h.since_ms,
            }),
            holders: u32::try_from(self.holders.len()).unwrap_or(u32::MAX),
            defaulted_at_least: oldest.and_then(|(_, h)| h.defaulted_at_least()),
        }
    }

    fn oldest(&self) -> Option<(&String, &Holder)> {
        self.holders.iter().min_by_key(|(_, h)| h.seq)
    }
}

/// "process → host" for the WS message: control and bidi characters removed,
/// each part truncated, nothing escaped.
pub fn plain_summary(process: &str, host: &str) -> String {
    fn part(text: &str) -> String {
        let clean = crate::translator::verdict::strip_display_hazards(text);
        let mut out: String = clean.chars().take(PART_MAX_CHARS).collect();
        if clean.chars().count() > PART_MAX_CHARS {
            out.push('…');
        }
        out
    }
    format!("{} → {}", part(process), part(host))
}

const PART_MAX_CHARS: usize = 64;

/// The bridge's shared prompt slot: every change is broadcast as a
/// `PromptSlot` message, and a release that cost at least one defaulted
/// connection sends `Notice::PromptSlotSummary`. Uses a std mutex: `release`
/// runs from `Drop` and never awaits.
#[derive(Clone)]
pub struct PromptSlotHandle {
    slot: Arc<Mutex<PromptSlot>>,
    broadcast: broadcast::Sender<ServerMessage>,
    notices: Arc<NoticeBus>,
}

impl PromptSlotHandle {
    pub fn new(broadcast: broadcast::Sender<ServerMessage>, notices: Arc<NoticeBus>) -> Self {
        Self {
            slot: Arc::default(),
            broadcast,
            notices,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PromptSlot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Marks `row_id` as holding the slot and announces it.
    ///
    /// Every announcement is sent while the slot lock is held, so concurrent
    /// changes (a ping racing a verdict, two Asks) reach clients in the order
    /// they happened, the way `ask_rule` publishes a row's insertion under
    /// the settlement mutex. Clients keep only the latest state, so a stale
    /// message sent last would leave an answered prompt on screen. Both sends
    /// are synchronous and never block.
    pub fn hold(&self, row_id: &str, what: String) {
        let mut slot = self.lock();
        slot.hold(row_id, what, now_ms());
        let _ = self.broadcast.send(slot.message());
    }

    /// Releases `row_id` (whatever ended the prompt) and announces it.
    /// `ask_id` keys the summary notice like the other per-prompt notices.
    pub fn release(&self, row_id: &str, ask_id: u64) {
        let mut slot = self.lock();
        let Some(count) = slot.release(row_id) else {
            return;
        };
        let _ = self.broadcast.send(slot.message());
        if let Some(count) = count.filter(|n| *n > 0) {
            self.notices.send(Notice::PromptSlotSummary {
                row_id: ask_id,
                count,
            });
        }
    }

    /// A ping's counters. Announces only when the broadcast state changed.
    pub fn observe(&self, rule_misses: u64, uptime: u64, rules_generation: u64) {
        let mut slot = self.lock();
        let before = slot.message();
        slot.observe(rule_misses, uptime, rules_generation);
        let after = slot.message();
        if after != before {
            let _ = self.broadcast.send(after);
        }
    }

    /// The current state, for a `RequestSnapshot` answer.
    pub fn message(&self) -> ServerMessage {
        self.lock().message()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "prompt_slot/tests.rs"]
mod tests;
