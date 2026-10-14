//! A service account found at another uid than its copy in the firewall
//! names (#155a as reshaped by the tower's r13 facts, 2026-10-14): the
//! `systemd-resolve` and `chrony` uids are allocated when the image is
//! built, so an image update can move them. Only a confirmed lookup says
//! so; an unconfirmed one withholds (`manager_resolver.rs`).
//!
//! Owner decision (2026-10-14): an opted-in copy is re-pinned in place to
//! the account's new uid ([`CuratedAction::Upgrade`], one `CHANGE_RULE` of
//! the same name, so the rule is never missing and both uids are allowed
//! for at most that one command), with no new consent: it is the same
//! account, and moving it needs root. DNS keeps its grant, which names the
//! account, not a uid; without one its copy waits for a confirmation. A
//! refused rewrite takes #130's path (`manager_upgrade.rs`). Also:
//! - an undecided copy is left as it is, with a reason (re-review M1);
//! - a copy the user turned off is deleted, as any other;
//! - a moved copy is never turned on: its queued re-enables are dropped,
//!   and a Rules-page turn-on is refused (`send_managed_toggle`), as the
//!   old uid may now be another account's. Turning it off stays allowed.

use snitchwatch_proto::protocol::Rule;

use super::resolver::{account_uid, Found};
use super::state::State;
use super::*;

/// DNS whose account moved, without a grant to rewrite it under.
pub const DNS_MOVED_REASON: &str = "The system resolver's account (systemd-resolve) now has a \
     different user ID than the copy of this rule in the firewall names, so that copy no longer \
     matches it. Confirm the DNS rule again (Review DNS choice) and Snitchwatch updates it.";

/// An undecided copy whose account moved.
pub const MOVED_UNDECIDED_REASON: &str = "This service's account now has a different user ID \
     than the copy of this rule in the firewall names, so the copy may not match the service. \
     It is left as it is until you turn this on or off.";

/// An entry whose copy was re-pinned to its account's new uid.
pub const REPINNED_NOTE: &str = "This service's account has a new user ID on this computer, \
     so Snitchwatch updated the rule to match it.";

/// The entries whose unedited copy in `rules` names another uid than
/// their account was found at.
pub(super) fn moved(found: &Found, rules: &BTreeMap<String, Rule>) -> BTreeSet<String> {
    entries()
        .iter()
        .filter(|entry| {
            let Some(uid) = account_uid(entry, found) else {
                return false;
            };
            rules
                .get(&entry.rule_name())
                .filter(|rule| is_unedited(Some(entry), None, rule))
                .and_then(super::super::sender_uid)
                .is_some_and(|named| named != uid)
        })
        .map(|entry| entry.id.clone())
        .collect()
}

impl CuratedDefaults {
    /// Act on the entries whose account `moved` this pass; returns the
    /// rewrites to send.
    pub(super) fn handle_moved(
        &self,
        state: &mut State,
        moved: &BTreeSet<String>,
        rules: &BTreeMap<String, Rule>,
    ) -> Vec<CuratedAction> {
        state.moved = moved.clone();
        let mut actions = Vec::new();
        for id in moved {
            state.reenables.remove(id);
            let undecided = matches!(
                state.statuses.get(id),
                Some(EntryStatus::InFirewall | EntryStatus::InFirewallButOff)
            );
            if undecided {
                state.problems.insert(id.clone(), MOVED_UNDECIDED_REASON);
                continue;
            }
            if !state.choices.enabled.contains(id) {
                continue;
            }
            let Some(entry) = entries().iter().find(|entry| &entry.id == id) else {
                continue;
            };
            let active = rules.get(&entry.rule_name()).is_some_and(|r| r.enabled);
            if entry.broad() && active && !state.choices.has_broad_authority(id) {
                state.problems.insert(id.clone(), DNS_MOVED_REASON);
            } else {
                actions.push(CuratedAction::Upgrade(id.clone()));
            }
        }
        actions
    }
}
