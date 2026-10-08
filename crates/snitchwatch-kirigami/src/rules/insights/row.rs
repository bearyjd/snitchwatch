//! What one row of the Rules tab shows of the insights, as plain strings the
//! model hands to QML.

use super::hit_badge::{hit_badge, HitBadge, UNUSED_WINDOW_MS};
use super::shadow::FindingKind;
use super::state::AnalysisState;
use crate::rules::hits::RuleHitsView;
use crate::rules::row_store::Rule;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RowInsights {
    /// `unused`, `since`, `sinceMissed` (a gap of unknown time), or empty.
    pub hit_badge_kind: &'static str,
    /// The time a `since` badge counts from, in Unix milliseconds; else 0.
    pub hit_badge_ms: f64,
    /// `redundant`, `neverApplies`, `maybeShadowed`, or empty.
    pub shadow_kind: &'static str,
    /// Plain text, empty without a finding.
    pub shadow_text: String,
    /// The covering rule's name, to jump to its row; empty without a finding.
    pub shadow_by: String,
}

pub fn row_insights(
    rule: &Rule,
    hits: &RuleHitsView,
    analysis: &AnalysisState,
    now_ms: i64,
) -> RowInsights {
    let mut row = RowInsights::default();
    match hit_badge(rule, hits, now_ms, UNUSED_WINDOW_MS) {
        None => {}
        Some(HitBadge::Unused) => row.hit_badge_kind = "unused",
        Some(HitBadge::Since {
            since_unix_ms,
            lossy,
        }) => {
            row.hit_badge_kind = if lossy { "sinceMissed" } else { "since" };
            row.hit_badge_ms = since_unix_ms as f64;
        }
    }
    if let Some(finding) = analysis.finding(&rule.name) {
        row.shadow_kind = match finding.kind {
            FindingKind::Redundant => "redundant",
            FindingKind::NeverApplies => "neverApplies",
            FindingKind::MayBeShadowed => "maybeShadowed",
        };
        row.shadow_text = finding.text();
        row.shadow_by = finding.by.clone();
    }
    row
}
