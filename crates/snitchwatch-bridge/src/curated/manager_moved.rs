//! A service account found at another uid than its copy in the firewall
//! names (#155a as reshaped by the tower's r13 facts, 2026-10-14): the
//! `systemd-resolve` and `chrony` uids are allocated when the image is
//! built, so an image update can move them. Only a confirmed lookup says
//! so; an unconfirmed one withholds (`manager_resolver.rs`).
//!
//! Whatever the switch below says:
//! - an undecided copy is left as it is, with a reason (re-review M1);
//! - a copy the user turned off is deleted, as any other;
//! - a moved copy is never turned on: its queued re-enables are dropped,
//!   and a Rules-page turn-on is refused (`send_managed_toggle`), as the
//!   old uid may now be another account's. Turning it off stays allowed.

use snitchwatch_proto::protocol::Rule;

use super::resolver::{account_uid, Found};
use super::state::{keep, State};
use super::*;

/// What happens to an opted-in copy whose account moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UidChange {
    /// (A) Rewrite it in place at the new uid, keeping `enabled` and
    /// `created`, with no new consent: it is the same account, and changing
    /// passwd needs root. DNS still needs its live grant (#128).
    Rewrite,
    /// (B) Turn the entry off, its grant revoked, and say why: the next
    /// pass deletes the copy, and turning it on again asks again (DNS: a
    /// new ticket). Under a deny default DNS stays blocked until then.
    TurnOff,
}

/// PLACEHOLDER, owner decision pending (#120, 2026-10-14): A until the
/// owner confirms A or B.
pub const ON_UID_CHANGE: UidChange = UidChange::Rewrite;

/// (A) DNS whose account moved, without a live grant to rewrite it under.
pub const DNS_MOVED_REASON: &str = "The system resolver's account (systemd-resolve) now has a \
     different user ID than the copy of this rule in the firewall names, so that copy no longer \
     matches it. Confirm the DNS rule again (Review DNS choice) and Snitchwatch updates it.";

/// An undecided copy whose account moved.
pub const MOVED_UNDECIDED_REASON: &str = "This service's account now has a different user ID \
     than the copy of this rule in the firewall names, so the copy may not match the service. \
     It is left as it is until you turn this on or off.";

/// (B) An entry turned off because its account moved.
pub const UID_CHANGED_OFF_REASON: &str = "This service's account now has a different user ID \
     than its rule named, so Snitchwatch turned the rule off. Turn it on to add it again for \
     the account's new user ID.";

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
    /// Act on the entries whose account `moved` this pass, as
    /// [`ON_UID_CHANGE`] says; returns the rewrites to send.
    pub(super) fn handle_moved(
        &self,
        state: &mut State,
        moved: &BTreeSet<String>,
        rules: &BTreeMap<String, Rule>,
        generation: u64,
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
            match state.uid_change {
                UidChange::Rewrite => {
                    let active = rules.get(&entry.rule_name()).is_some_and(|r| r.enabled);
                    if entry.broad() && active && !state.choices.has_broad_authority(id) {
                        state.problems.insert(id.clone(), DNS_MOVED_REASON);
                    } else {
                        actions.push(CuratedAction::Upgrade(id.clone()));
                    }
                }
                UidChange::TurnOff => {
                    warn!(entry = %id, "a recommended rule's account moved; turned off");
                    self.revoke_locked(state, id);
                    let choices = state.choices.disable(id);
                    keep(state, choices);
                    state
                        .install_refusals
                        .insert(id.clone(), (UID_CHANGED_OFF_REASON, generation));
                }
            }
        }
        actions
    }
}
