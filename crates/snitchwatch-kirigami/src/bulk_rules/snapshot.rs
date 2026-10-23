//! What the sequencer knows of the rules list, from the bridge's
//! `SetRules`, `UpdateRules` and `RulesNotShown`.
//!
//! A list counts as READ only after a `SetRules` AND a `RulesNotShown`
//! saying `listed: true` have arrived since the last request. While the
//! bridge has no daemon list it broadcasts an empty `SetRules` together
//! with `listed: false`; that empty list is never read as "no such rule".

use snitchwatch_bridge::ws_messages::ServerMessage;

use crate::rules::row_store::{Rule, RulesStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Ready,
    Unavailable(String),
    Waiting,
}

#[derive(Debug, Default)]
pub struct ListView {
    store: RulesStore,
    set_seen: bool,
    /// `(listed, over_limit)` of the latest `RulesNotShown`.
    not_shown: Option<(bool, bool)>,
    too_large: u32,
    unreadable: u32,
    /// The bridge says the list may not match the firewall service's.
    doubtful: bool,
}

impl ListView {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new list is wanted: what arrives before it no longer counts.
    pub fn expect_fresh(&mut self) {
        self.set_seen = false;
        self.not_shown = None;
    }

    pub fn absorb(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::SetRules { rules } => {
                self.set_seen = true;
                // The "listed" word that goes with a list follows it: until
                // it does, this list is not trusted (a withdrawal is an
                // empty list first).
                self.not_shown = None;
                self.unreadable = unreadable(rules);
                self.store.apply(message);
            }
            ServerMessage::UpdateRules { rules } => {
                self.unreadable += unreadable(rules);
                self.store.apply(message);
            }
            ServerMessage::RulesNotShown {
                too_large,
                over_limit_total,
                listed,
                left_on_disk,
                count_mismatch,
            } => {
                self.doubtful = *left_on_disk > 0 || *count_mismatch;
                self.too_large = *too_large;
                self.not_shown = Some((*listed, over_limit_total.is_some()));
            }
            _ => {}
        }
    }

    pub fn listing(&self) -> Listing {
        match self.not_shown {
            Some((_, true)) => Listing::Unavailable(
                "The firewall service has more rules than Snitchwatch reads.".into(),
            ),
            Some((true, false)) if self.set_seen => Listing::Ready,
            _ => Listing::Waiting,
        }
    }

    /// Why no list is read yet, once the wait is over.
    pub fn why_waiting(&self) -> String {
        match self.not_shown {
            Some((false, _)) => "The firewall service has not sent its rules yet.".into(),
            _ => "No rules list arrived in time.".into(),
        }
    }

    /// The rules read so far.
    pub fn rules(&self) -> &[Rule] {
        self.store.rules()
    }

    pub fn find(&self, name: &str) -> Option<&Rule> {
        self.store.find_by_name(name)
    }

    /// The list leaves rules out (too large, unreadable, deleted but still
    /// on disk, or a count that differs), so a name that is not in it may
    /// still exist.
    pub fn left_out(&self) -> bool {
        self.too_large > 0 || self.unreadable > 0 || self.doubtful
    }
}

fn unreadable(rules: &[serde_json::Value]) -> u32 {
    let bad = rules
        .iter()
        .filter(|v| serde_json::from_value::<Rule>((*v).clone()).is_err())
        .count();
    u32::try_from(bad).unwrap_or(u32::MAX)
}
