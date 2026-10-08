//! What to do about the curated defaults, given the daemon's rules and the
//! user's choices (prompt-slot plan Part D, item 13). Pure: the caller
//! sends the commands and records their replies.
//!
//! - An entry the user turned on that the daemon lacks is installed.
//! - Once installed, an entry that disappears was deleted outside
//!   Snitchwatch. It is recorded and **never reinstalled** until the user
//!   turns it on again (#62's requirement; plan item 13).
//! - An entry the user turned off is deleted, but only an **unedited** copy:
//!   same as what was installed, apart from `enabled`. A copy someone
//!   edited is left alone and flagged, even on opt-out.
//! - A rule under the prefix that is no longer in the data file is deleted
//!   only if it is a copy Snitchwatch installed and nobody edited.
//! - Nothing outside the prefix is ever touched.
//!
//! Callers run this only on a known rule list (never while the rules cache
//! is `Unknown`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::{Operator, Rule};

use super::store::{Choices, InstalledCopy};
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
    /// The daemon's rule differs from what Snitchwatch installed (apart from
    /// `enabled`): left alone.
    EditedByYou,
    /// Installed once, then deleted outside Snitchwatch: not reinstalled
    /// until turned on again.
    DeletedOutside,
    /// Turned on, but the install failed (see the entry's problem).
    NotInstalled,
    /// Turned off, but the delete failed (see the entry's problem).
    NotRemoved,
    /// A status from a newer bridge.
    #[serde(other)]
    Unknown,
}

/// What reconcile decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The choices to keep: installed copies adopted or forgotten, and
    /// deletions noticed.
    pub choices: Choices,
    pub actions: Vec<CuratedAction>,
    pub statuses: BTreeMap<String, EntryStatus>,
}

/// Plan reconcile for `entries` against the daemon's `rules`.
pub fn plan(entries: &[CuratedEntry], rules: &BTreeMap<String, Rule>, choices: &Choices) -> Plan {
    let mut next = choices.clone();
    let mut actions = Vec::new();
    let mut statuses = BTreeMap::new();
    for entry in entries {
        let status = plan_entry(entry, rules, choices, &mut next, &mut actions);
        statuses.insert(entry.id.clone(), status);
    }
    for (name, rule) in rules {
        let Some(id) = name.strip_prefix(CURATED_DEFAULT_RULE_NAME_PREFIX) else {
            continue;
        };
        if entries.iter().any(|entry| entry.id == id) {
            continue;
        }
        // No longer offered: delete it only if it is our unedited copy.
        if choices
            .installed
            .get(id)
            .is_some_and(|copy| copy.matches(rule))
        {
            actions.push(CuratedAction::Delete {
                id: id.to_string(),
                name: name.clone(),
            });
        }
    }
    // Forget copies of entries that are gone from both the file and the
    // daemon.
    next.installed.retain(|id, copy| {
        entries.iter().any(|entry| &entry.id == id) || rules.contains_key(&copy.name)
    });
    Plan {
        choices: next,
        actions,
        statuses,
    }
}

fn plan_entry(
    entry: &CuratedEntry,
    rules: &BTreeMap<String, Rule>,
    choices: &Choices,
    next: &mut Choices,
    actions: &mut Vec<CuratedAction>,
) -> EntryStatus {
    let name = entry.rule_name();
    let enabled = choices.enabled.contains(&entry.id);
    let Some(present) = rules.get(&name) else {
        let was_installed = next.installed.remove(&entry.id).is_some();
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
    let reference = choices
        .installed
        .get(&entry.id)
        .cloned()
        .unwrap_or_else(|| InstalledCopy::of(&entry.rule()));
    if !reference.matches(present) {
        return EntryStatus::EditedByYou;
    }
    // Our unedited copy (adopted if the record was lost).
    next.installed.insert(entry.id.clone(), reference);
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

/// A rule's meaning apart from `enabled` and `created`: what it does, for
/// how long, and its conditions as a set (the daemon may report a list's
/// members in another order, and fills a list's `data`).
pub(crate) fn same_ignoring_enabled(a: &Rule, b: &Rule) -> bool {
    shape(a) == shape(b)
}

type Leaf = (String, String, String, bool);

fn shape(rule: &Rule) -> (&str, &str, &str, bool, bool, Vec<Leaf>) {
    let mut leaves: Vec<Leaf> = match &rule.operator {
        None => Vec::new(),
        Some(op) if op.r#type == "list" => op.list.iter().map(leaf_shape).collect(),
        Some(op) => vec![leaf_shape(op)],
    };
    leaves.sort();
    (
        rule.action.as_str(),
        rule.duration.as_str(),
        rule.description.as_str(),
        rule.precedence,
        rule.nolog,
        leaves,
    )
}

fn leaf_shape(op: &Operator) -> Leaf {
    (
        op.r#type.clone(),
        op.operand.clone(),
        op.data.clone(),
        op.sensitive,
    )
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
