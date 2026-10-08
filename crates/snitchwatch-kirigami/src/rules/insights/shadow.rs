//! Rules that can never decide a connection: static analysis of the cached
//! rule list (P2.6 Part 2). Hit counts can't show this, because only the
//! *deciding* rule is ever counted.
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
//! **What is claimed, and what is not.** Only that `B` never decides, and that
//! the named rule `A` matches every connection `B` does and takes precedence
//! over it. *Who* decides those connections, and with what verdict, is not
//! claimed: a third rule matching only some of them (a precedence allow on
//! the same host and one port, say) can decide those, and so can a stop rule
//! earlier than `A`. Removing a rule that never decides changes no verdict,
//! but what the connections get instead is up to the whole list.
//!
//! **Which rule is named.** For a non-stop `B`, the earliest covering stop rule
//! (it ends the scan for every connection `B` matches, before any later
//! rule), else the last covering non-stop rule (what replaces `B`). For a
//! stop `B`, the earliest covering stop rule.
//!
//! **Only what can be proven is claimed.** `A` must be permanent (`always`:
//! `until restart` and timed rules end, and with them the shadowing), enabled,
//! and made of conditions [`super::atoms`] models. A proof that rests on
//! comparisons reproduced exactly is [`FindingKind::NeverDecides`]; one that
//! also rests on the simulator's regular-expression engine is only
//! [`FindingKind::MayBeShadowed`], because Go's RE2 differs from it for rare
//! constructs. No finding does not mean no shadowing. Rules managed elsewhere
//! (blocklists, the packaged `000-snitchwatch-` rules, anything read-only)
//! get no finding, though they may be the rule that shadows.
//!
//! The analysis changes nothing: it reads the list and reports.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::atoms::{Conjunction, Proof};
use super::is_managed;
use crate::rules::row_store::Rule;
use crate::rules::simulator::stops_scan;

/// Most enabled rules analysed. The pass is O(n²); #48 allows 10 000 rules.
pub const MAX_ANALYZED_RULES: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// The rule never decides; `by` matches every connection it does and
    /// takes precedence.
    NeverDecides,
    /// The same, but the proof rests on the simulator's regular-expression
    /// engine.
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
    /// Plain text; says what was found and nothing about acting on it, and
    /// nothing about what the connections get instead.
    pub fn text(&self) -> String {
        let by = &self.by_display;
        match self.kind {
            FindingKind::NeverDecides => format!(
                "Never decides: {by} matches every connection this rule does and takes precedence."
            ),
            FindingKind::MayBeShadowed => format!(
                "May never decide: {by} appears to match every connection this rule does \
                 and take precedence."
            ),
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
    analyze_until(rules, &AtomicBool::new(false)).expect("an analysis nobody cancels finishes")
}

/// [`analyze`], stopping early (with `None`) once `cancel` is set: a worker
/// whose rule list has since changed isn't worth finishing.
pub fn analyze_until(rules: &[Rule], cancel: &AtomicBool) -> Option<Analysis> {
    let mut active: Vec<&Rule> = rules.iter().filter(|r| r.enabled).collect();
    if active.len() > MAX_ANALYZED_RULES {
        return Some(Analysis::TooMany {
            enabled: active.len(),
            limit: MAX_ANALYZED_RULES,
        });
    }
    active.sort_by(|a, b| a.name.cmp(&b.name));
    let conditions: Vec<Conjunction> = active.iter().map(|r| Conjunction::from_rule(r)).collect();

    let mut findings = BTreeMap::new();
    for (position, &rule) in active.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if is_managed(rule) {
            continue;
        }
        if let Some(finding) = shadowed_by(position, &active, &conditions) {
            findings.insert(rule.name.clone(), finding);
        }
    }
    Some(Analysis::Done { findings })
}

/// A rule that covers the shadowed one and may shadow it.
struct Covering {
    proof: Proof,
    position: usize,
    stops: bool,
}

/// The rule to name for `active[position]`, if one provably shadows it: of
/// the strongest proofs, see the module docs for which.
fn shadowed_by(position: usize, active: &[&Rule], conditions: &[Conjunction]) -> Option<Finding> {
    let shadowed = active[position];
    let shadowed_stops = stops_scan(shadowed);
    let mut covering = Vec::new();
    for (other, &candidate) in active.iter().enumerate() {
        if other == position || candidate.duration != "always" || !conditions[other].is_modelled() {
            continue;
        }
        let stops = stops_scan(candidate);
        let can_shadow = if stops {
            !shadowed_stops || other < position
        } else {
            !shadowed_stops && other > position
        };
        if !can_shadow {
            continue;
        }
        if let Some(proof) = conditions[other].covers(&conditions[position]) {
            covering.push(Covering {
                proof,
                position: other,
                stops,
            });
        }
    }
    let strongest = covering.iter().map(|c| c.proof).min()?;
    let tier = || covering.iter().filter(|c| c.proof == strongest);
    // A stop rule ends the scan for everything the shadowed rule matches, at
    // the earliest such rule; with none, the last non-stop rule replaces it.
    let named = tier()
        .filter(|c| c.stops)
        .min_by_key(|c| c.position)
        .or_else(|| tier().max_by_key(|c| c.position))?;
    let by = active[named.position];
    Some(Finding {
        kind: match strongest {
            Proof::Exact => FindingKind::NeverDecides,
            Proof::Engine => FindingKind::MayBeShadowed,
        },
        by: by.name.clone(),
        by_display: by.shown_name().to_string(),
    })
}
