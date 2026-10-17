//! Rules under the curated prefix that nothing offers or owns any more
//! (#113, "No longer recommended"): no current entry has their id, and
//! Snitchwatch recorded no copy of them, so reconcile never deletes them,
//! the Recommended page lists no entry for them, and the Rules page refuses
//! every command under the prefix. Pure: the manager lists them and, only on
//! the user's confirmed request, deletes one that is still an orphan.

use std::collections::{BTreeMap, BTreeSet};

use snitchwatch_proto::protocol::Rule;

use super::store::Choices;
use super::{valid_id, CuratedEntry};
use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;

/// Whether the rule `name` is an orphan: listed (readable, not left out),
/// under the curated prefix with an id the curated delete accepts, in no
/// current entry, and with no copy Snitchwatch recorded installing (that
/// one is the plan's to delete, and only unedited).
pub fn is_orphan(
    name: &str,
    entries: &[CuratedEntry],
    rules: &BTreeMap<String, Rule>,
    left_out: &BTreeSet<String>,
    choices: &Choices,
) -> bool {
    orphan_id(name).is_some_and(|id| {
        rules.contains_key(name)
            && !left_out.contains(name)
            && !entries.iter().any(|entry| entry.id == id)
            && !choices.was_installed(id)
    })
}

/// The orphans in `rules`, by id, each with whether it is enabled.
pub fn orphans(
    entries: &[CuratedEntry],
    rules: &BTreeMap<String, Rule>,
    left_out: &BTreeSet<String>,
    choices: &Choices,
) -> Vec<(String, bool)> {
    rules
        .iter()
        .filter(|(name, _)| is_orphan(name, entries, rules, left_out, choices))
        .filter_map(|(name, rule)| orphan_id(name).map(|id| (id.to_string(), rule.enabled)))
        .collect()
}

/// The id of `name` under the curated prefix, if it is one the curated
/// delete accepts.
fn orphan_id(name: &str) -> Option<&str> {
    name.strip_prefix(CURATED_DEFAULT_RULE_NAME_PREFIX)
        .filter(|id| valid_id(id))
}

#[cfg(test)]
#[path = "orphans_tests.rs"]
mod tests;
