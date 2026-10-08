//! Shadowed and redundant rules: static analysis of the cached rule list
//! (P2.6 Part 2). Hit counts can't show this, because only the *deciding*
//! rule is ever counted.
//!
//! The scan is the daemon's (`Loader.FindFirstMatch`, mirrored by
//! [`crate::rules::simulator`]): enabled rules in **name** order; each match
//! is remembered, and a *stop rule* (deny, reject, or `precedence`) ends the
//! scan, otherwise a later match replaces an earlier one. So a rule `B` can
//! never be the one that decides when some rule `A` that covers it (every
//! connection `B` matches also matches `A`, see [`super::atoms`]) is:
//!
//! - a stop rule, at **any** position, when `B` does not stop the scan (if `A`
//!   comes first it ends the scan; if later it replaces `B`);
//! - a stop rule **earlier** than `B`, when `B` is a stop rule too;
//! - a **later** non-stop rule, when `B` is non-stop (it replaces `B`).
//!
//! An earlier non-stop rule never shadows a later one (the later one
//! replaces it), and a later stop rule never shadows an earlier stop rule.
//!
//! **Only what can be proven is claimed.** `A` must be permanent (`always`:
//! `until restart` and timed rules end, and with them the shadowing), enabled,
//! and made of conditions [`super::atoms`] models. A proof that rests on
//! comparisons reproduced exactly is a [`FindingKind::Redundant`] (the actions
//! match) or [`FindingKind::NeverApplies`]; one that also rests on the
//! simulator's regular-expression engine is only
//! [`FindingKind::MayBeShadowed`], because Go's RE2 differs from it for rare
//! constructs. No finding does not mean no shadowing. Rules managed elsewhere
//! (blocklists, the packaged `000-snitchwatch-` rules, anything read-only)
//! get no finding, though they may be the rule that shadows.
//!
//! The analysis changes nothing: it reads the list and reports.

use std::collections::BTreeMap;

use super::atoms::{Conjunction, Proof};
use super::is_managed;
use crate::rules::row_store::Rule;
use crate::rules::simulator::{daemon_action, stops_scan};

/// Most enabled rules analysed. The pass is O(n²); #48 allows 10 000 rules.
pub const MAX_ANALYZED_RULES: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// `by` already decides these connections the same way.
    Redundant,
    /// `by` decides these connections instead, differently.
    NeverApplies,
    /// `by` may decide these connections instead (an engine-dependent proof).
    MayBeShadowed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: FindingKind,
    /// The covering rule's name (its identity, to find its row).
    pub by: String,
    /// The covering rule's display name.
    pub by_display: String,
}

impl Finding {
    /// Plain text; says what was found and nothing about acting on it.
    pub fn text(&self) -> String {
        let by = &self.by_display;
        match self.kind {
            FindingKind::Redundant => {
                format!("Redundant: {by} already decides these connections the same way.")
            }
            FindingKind::NeverApplies => {
                format!("Never applies: {by} decides these connections instead.")
            }
            FindingKind::MayBeShadowed => format!("May be shadowed by {by}."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Analysis {
    /// Findings by the shadowed rule's name.
    Done { findings: BTreeMap<String, Finding> },
    /// More enabled rules than [`MAX_ANALYZED_RULES`]; nothing was analysed.
    TooMany { enabled: usize, limit: usize },
}

pub fn analyze(rules: &[Rule]) -> Analysis {
    let mut active: Vec<&Rule> = rules.iter().filter(|r| r.enabled).collect();
    if active.len() > MAX_ANALYZED_RULES {
        return Analysis::TooMany {
            enabled: active.len(),
            limit: MAX_ANALYZED_RULES,
        };
    }
    active.sort_by(|a, b| a.name.cmp(&b.name));
    let conditions: Vec<Conjunction> = active.iter().map(|r| Conjunction::from_rule(r)).collect();

    let mut findings = BTreeMap::new();
    for (position, &rule) in active.iter().enumerate() {
        if is_managed(rule) {
            continue;
        }
        if let Some(finding) = shadowed_by(position, &active, &conditions) {
            findings.insert(rule.name.clone(), finding);
        }
    }
    Analysis::Done { findings }
}

/// The best rule that shadows `active[position]`: the strongest proof, and
/// among equals the earliest.
fn shadowed_by(position: usize, active: &[&Rule], conditions: &[Conjunction]) -> Option<Finding> {
    let shadowed = active[position];
    let shadowed_stops = stops_scan(shadowed);
    let mut best: Option<(Proof, usize)> = None;
    for (other, &candidate) in active.iter().enumerate() {
        if other == position || candidate.duration != "always" || !conditions[other].is_modelled() {
            continue;
        }
        let can_shadow = if stops_scan(candidate) {
            !shadowed_stops || other < position
        } else {
            !shadowed_stops && other > position
        };
        if !can_shadow {
            continue;
        }
        let Some(proof) = conditions[other].covers(&conditions[position]) else {
            continue;
        };
        if best.is_none_or(|(strongest, _)| proof < strongest) {
            best = Some((proof, other));
        }
    }
    let (proof, other) = best?;
    let by = active[other];
    let kind = match proof {
        Proof::Engine => FindingKind::MayBeShadowed,
        Proof::Exact if daemon_action(by) == daemon_action(shadowed) => FindingKind::Redundant,
        Proof::Exact => FindingKind::NeverApplies,
    };
    Some(Finding {
        kind,
        by: by.name.clone(),
        by_display: by.shown_name().to_string(),
    })
}
