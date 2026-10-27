//! Which rows are "handled" and may collapse into the Connections view's
//! "Background and system" group (clarity phase 0, item 0c; plan
//! `docs/superpowers/plans/2026-10-27-exception-first-default.md`).
//!
//! A row is handled only if ALL hold: it is not deferred and not decided by
//! the firewall's default action; its verdict is allow; the rule that matched
//! it is a recommended rule (reserved `snitchwatch-default-` prefix) that is
//! in the CURRENT rules list and enabled; and its program has been seen
//! before. Everything else needs attention. Recommended rules match an exact
//! `/usr` executable path, so a program cannot reach this by choosing its
//! name; user rules and once-off answers never count. A friendly name is
//! never an input.

use std::collections::{HashMap, HashSet};

use snitchwatch_bridge::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use snitchwatch_bridge::ws_messages::ConnectionRow;

use super::row_store::{matched_rule_name, Verdict};
use crate::rules::row_store::Rule;

/// What the "handled" decision reads besides the row itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttentionFacts {
    /// Enabled recommended rules in the current rules list, by name.
    recommended: HashSet<String>,
    /// Executable paths seen before (and not new this session).
    seen: HashSet<String>,
}

impl AttentionFacts {
    pub fn new<'a>(rules: &[Rule], seen: impl IntoIterator<Item = &'a str>) -> Self {
        let recommended = rules
            .iter()
            .filter(|rule| rule.enabled && rule.name.starts_with(CURATED_DEFAULT_RULE_NAME_PREFIX))
            .map(|rule| rule.name.clone())
            .collect();
        Self {
            recommended,
            seen: seen.into_iter().map(str::to_string).collect(),
        }
    }

    /// Whether any row could be handled. False while the rules list is empty
    /// or unknown: then nothing collapses (fail open).
    pub fn collapses_anything(&self) -> bool {
        !self.recommended.is_empty()
    }

    /// Whether `row` is handled (see the module docs).
    pub fn is_handled(&self, row: &ConnectionRow) -> bool {
        // `matched_rule_name` is empty for a row the default action decided
        // (`decided_by_default`), whatever rule it names, so such a row never
        // matches a recommended rule.
        !row.deferred
            && Verdict::of(row) == Verdict::Allowed
            && self.recommended.contains(matched_rule_name(row))
            && row
                .process_path
                .as_deref()
                .is_some_and(|path| self.seen.contains(path))
    }
}

/// The group's one-line summary figures, kept per row as deltas arrive
/// (like `grouping::counts`) instead of scanning the store. Kept only while
/// collapsing is on; [`Self::rebuild`] when the facts or the switch change.
#[derive(Debug, Default)]
pub struct HandledCounts {
    on: bool,
    /// Handled row id -> its program's path.
    rows: HashMap<String, String>,
    /// Handled rows per program path.
    programs: HashMap<String, usize>,
}

impl HandledCounts {
    pub fn rebuild(&mut self, on: bool, facts: Option<&AttentionFacts>, rows: &[ConnectionRow]) {
        self.clear();
        self.on = on;
        for row in rows {
            self.upsert(facts, row);
        }
    }

    pub fn upsert(&mut self, facts: Option<&AttentionFacts>, row: &ConnectionRow) {
        if !self.on {
            return;
        }
        self.remove(&row.id);
        if !facts.is_some_and(|facts| facts.is_handled(row)) {
            return;
        }
        let Some(path) = row.process_path.clone() else {
            return;
        };
        *self.programs.entry(path.clone()).or_default() += 1;
        self.rows.insert(row.id.clone(), path);
    }

    pub fn remove(&mut self, id: &str) {
        let Some(path) = self.rows.remove(id) else {
            return;
        };
        if let Some(count) = self.programs.get_mut(&path) {
            *count -= 1;
            if *count == 0 {
                self.programs.remove(&path);
            }
        }
    }

    /// Empty, keeping whether it is on.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.programs.clear();
    }

    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    pub fn programs(&self) -> usize {
        self.programs.len()
    }
}

#[cfg(test)]
mod tests;
