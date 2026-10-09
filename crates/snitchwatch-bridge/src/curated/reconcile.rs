//! What to do about the curated defaults, given the daemon's rules and the
//! user's choices (prompt-slot plan Part D, item 13). Pure: the caller
//! sends the commands and records their replies.
//!
//! All of it is as of the firewall service's last rule list: the bridge
//! learns of a rule changed on disk only when the daemon reconnects or
//! confirms a command.
//! - An entry the user turned on that the daemon lacks is installed.
//! - Once installed, an entry that disappears was removed outside the
//!   Recommended page. It is recorded and **not reinstalled** until the
//!   user turns it off and on again (#62's requirement; plan item 13).
//! - An entry the user never chose (a first run) is left as it is: an
//!   unedited copy already in the firewall reads "In the firewall" until
//!   the user turns it on (adopt) or off (delete).
//! - An entry the user turned off is deleted, but only an **unedited** copy
//!   ([`is_unedited`]). A delete the daemon refused already took the rule
//!   out of its memory (`RulesCache::apply_refused`): the entry reads
//!   [`EntryStatus::OffFileLeft`] and nothing is sent until the file loads
//!   again and lists the rule, at the next daemon start. A copy someone edited is left alone and flagged,
//!   even on opt-out; so is a copy too large for the bridge's list (left
//!   out of the snapshot, security review L4).
//! - A rule under the prefix that is no longer in the data file is deleted
//!   only if it is a copy Snitchwatch recorded installing, unedited.
//! - Nothing outside the prefix is ever touched.
//!
//! Callers run this only on a known rule list (never while the rules cache
//! is `Unknown`), and only while the bridge may change rules
//! ([`inert_statuses`] otherwise).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::Rule;

use super::canonical::is_unedited;
use super::store::Choices;
use super::CuratedEntry;
use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;

/// A command to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CuratedAction {
    /// Install the entry with this id.
    Install(String),
    /// Delete the rule `name`, an unedited copy Snitchwatch installed.
    Delete { id: String, name: String },
}

/// Where an entry stands, for the GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryStatus {
    /// The daemon's rule list isn't known yet: nothing is done.
    Waiting,
    /// This bridge changes no recommended rules (see the message's
    /// `unavailable`), and the daemon has no rule under this name.
    Unavailable,
    /// The daemon has this entry's rule, unedited (added earlier) and
    /// enabled, and this bridge changes none, or the user hasn't chosen yet.
    InFirewall,
    /// The same, but the rule is turned off on the Rules page.
    InFirewallButOff,
    /// Not turned on, and not in the daemon.
    Off,
    /// Turned on; the rule is being installed.
    Installing,
    /// Installed and active.
    Installed,
    /// Installed, but turned off in the rule list.
    InstalledButOff,
    /// Turned off; the rule is being removed.
    Removing,
    /// The daemon's rule under this name differs from the entry (or is too
    /// large to read): left alone, and it still applies.
    EditedByYou,
    /// Installed once, then removed outside the Recommended page (or with
    /// its Remove button): not reinstalled until turned off and on again.
    DeletedOutside,
    /// Turned on, but the install failed (see the entry's problem).
    NotInstalled,
    /// Turned off, but the delete failed (see the entry's problem).
    NotRemoved,
    /// Turned off: the firewall service stopped using the rule but couldn't
    /// remove its saved file, which may bring it back when the service
    /// restarts (tower r12, `RulesCache::files_left`).
    OffFileLeft,
    /// A status from a newer bridge.
    #[serde(other)]
    Unknown,
}

/// The daemon's side of a plan: its rules, the names it has that were too
/// large to keep (`RulesCache::left_out`), and the names whose delete it
/// refused, whose files may remain (`RulesCache::files_left`).
#[derive(Debug, Clone, Copy)]
pub struct DaemonRules<'a> {
    pub rules: &'a BTreeMap<String, Rule>,
    pub left_out: &'a BTreeSet<String>,
    pub files_left: &'a BTreeSet<String>,
}

/// What reconcile decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The choices to keep: installed copies adopted or forgotten, and
    /// removals noticed.
    pub choices: Choices,
    pub actions: Vec<CuratedAction>,
    pub statuses: BTreeMap<String, EntryStatus>,
}

/// Plan reconcile for `entries` against the daemon's rules.
pub fn plan(entries: &[CuratedEntry], daemon: DaemonRules<'_>, choices: &Choices) -> Plan {
    let mut next = choices.clone();
    let mut actions = Vec::new();
    let mut statuses = BTreeMap::new();
    for entry in entries {
        let status = plan_entry(entry, daemon, choices, &mut next, &mut actions);
        statuses.insert(entry.id.clone(), status);
    }
    for (name, rule) in daemon.rules {
        let Some(id) = name.strip_prefix(CURATED_DEFAULT_RULE_NAME_PREFIX) else {
            continue;
        };
        if entries.iter().any(|entry| entry.id == id) {
            continue;
        }
        // No longer offered: delete it only if it is our unedited copy.
        if is_unedited(None, choices.installed.get(id), rule) {
            actions.push(CuratedAction::Delete {
                id: id.to_string(),
                name: name.clone(),
            });
        }
    }
    // Forget copies of entries that are gone from both the file and the
    // daemon.
    next.installed.retain(|id, copy| {
        entries.iter().any(|entry| &entry.id == id) || daemon.rules.contains_key(&copy.name)
    });
    next.installed_ids.retain(|id| {
        entries.iter().any(|entry| &entry.id == id)
            || daemon
                .rules
                .contains_key(&format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{id}"))
    });
    Plan {
        choices: next,
        actions,
        statuses,
    }
}

fn plan_entry(
    entry: &CuratedEntry,
    daemon: DaemonRules<'_>,
    choices: &Choices,
    next: &mut Choices,
    actions: &mut Vec<CuratedAction>,
) -> EntryStatus {
    let name = entry.rule_name();
    if daemon.left_out.contains(&name) {
        // Present but unreadable: never installed or deleted over.
        return EntryStatus::EditedByYou;
    }
    let enabled = choices.enabled.contains(&entry.id);
    let Some(present) = daemon.rules.get(&name) else {
        let was_installed = choices.was_installed(&entry.id);
        next.installed.remove(&entry.id);
        next.installed_ids.remove(&entry.id);
        if !enabled && daemon.files_left.contains(&name) {
            return EntryStatus::OffFileLeft;
        }
        if !enabled {
            return EntryStatus::Off;
        }
        if choices.deleted_by_user.contains(&entry.id) || was_installed {
            next.deleted_by_user.insert(entry.id.clone());
            return EntryStatus::DeletedOutside;
        }
        actions.push(CuratedAction::Install(entry.id.clone()));
        return EntryStatus::Installing;
    };
    if !is_unedited(Some(entry), None, present) {
        return EntryStatus::EditedByYou;
    }
    if !enabled && !choices.disabled.contains(&entry.id) {
        // Undecided (a first run, or a file that was moved away): an
        // unedited copy already there is left as it is until the user
        // turns it on (adopt) or off (delete). Re-review M1.
        return in_firewall(present);
    }
    // Our unedited copy (adopted if the record was lost).
    *next = next.installed(&entry.id, &entry.rule());
    next.deleted_by_user.remove(&entry.id);
    if !enabled {
        actions.push(CuratedAction::Delete {
            id: entry.id.clone(),
            name,
        });
        return EntryStatus::Removing;
    }
    if present.enabled {
        EntryStatus::Installed
    } else {
        EntryStatus::InstalledButOff
    }
}

/// Where each entry stands while the bridge changes nothing (the per-user
/// bridge, or choices it can't read or save): what the daemon has, and
/// nothing more.
pub fn inert_statuses(
    entries: &[CuratedEntry],
    daemon: DaemonRules<'_>,
) -> BTreeMap<String, EntryStatus> {
    entries
        .iter()
        .map(|entry| {
            let name = entry.rule_name();
            let status = match daemon.rules.get(&name) {
                _ if daemon.left_out.contains(&name) => EntryStatus::EditedByYou,
                Some(rule) if is_unedited(Some(entry), None, rule) => in_firewall(rule),
                Some(_) => EntryStatus::EditedByYou,
                None => EntryStatus::Unavailable,
            };
            (entry.id.clone(), status)
        })
        .collect()
}

/// An unedited copy left as it is, enabled or turned off on the Rules page
/// (re-review 2, M3: its switch then reads off).
fn in_firewall(rule: &Rule) -> EntryStatus {
    if rule.enabled {
        EntryStatus::InFirewall
    } else {
        EntryStatus::InFirewallButOff
    }
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
