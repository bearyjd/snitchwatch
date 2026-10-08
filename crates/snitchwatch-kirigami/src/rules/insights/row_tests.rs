use super::row::{row_insights, RowInsights};
use super::shadow::analyze;
use super::state::AnalysisState;
use super::testkit::*;
use crate::rules::hits::RuleHitsView;
use snitchwatch_bridge::ws_messages::{ServerMessage, StorageStatus};

const DAY: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_800_000_000_000;

fn counting(persistent: bool, lossy: bool) -> RuleHitsView {
    let mut view = RuleHitsView::default();
    view.apply(&ServerMessage::RuleHits {
        since_unix_ms: Some(NOW - 30 * DAY),
        lossy,
        last_gap_unix_ms: lossy.then_some(NOW - DAY),
        storage: StorageStatus {
            persistent,
            reason: None,
            unreadable: false,
        },
        hits: vec![],
    });
    view
}

fn analysed(rules: &[crate::rules::row_store::Rule]) -> AnalysisState {
    let mut state = AnalysisState::default();
    let generation = state.start().unwrap();
    state.finish(generation, analyze(rules));
    state
}

#[test]
fn a_row_carries_both_its_badge_and_its_finding() {
    let mut old = deny("100-deny", host("example.com"));
    old.created = (NOW - 40 * DAY) / 1000;
    let mut shadowed = allow("200-allow", host("example.com"));
    shadowed.created = (NOW - 40 * DAY) / 1000;
    let rules = [old, shadowed];
    let state = analysed(&rules);
    let hits = counting(true, false);

    let row = row_insights(&rules[1], &hits, &state, NOW);
    assert_eq!(row.hit_badge_kind, "unused");
    assert_eq!(row.shadow_kind, "neverApplies");
    assert_eq!(
        row.shadow_text,
        "Never applies: 100-deny decides these connections instead."
    );
    assert_eq!(row.shadow_by, "100-deny");

    let row = row_insights(&rules[0], &hits, &state, NOW);
    assert_eq!(row.shadow_kind, "");
    assert_eq!(row.shadow_text, "");
    assert_eq!(row.shadow_by, "");
}

#[test]
fn the_badge_kinds_name_what_the_row_may_claim() {
    let mut rule = allow("a", host("example.com"));
    rule.created = (NOW - 40 * DAY) / 1000;
    let idle = AnalysisState::default();
    let kind = |persistent, lossy| {
        let row = row_insights(&rule, &counting(persistent, lossy), &idle, NOW);
        (row.hit_badge_kind, row.hit_badge_ms)
    };
    assert_eq!(kind(true, false), ("unused", 0.0));
    assert_eq!(kind(true, true), ("missed", 0.0));
    assert_eq!(kind(false, false), ("since", (NOW - 30 * DAY) as f64));
    assert_eq!(kind(false, true), ("sinceMissed", (NOW - 30 * DAY) as f64));
}

#[test]
fn without_counts_or_an_analysis_a_row_shows_nothing() {
    let rule = allow("a", host("example.com"));
    let row = row_insights(
        &rule,
        &RuleHitsView::default(),
        &AnalysisState::default(),
        NOW,
    );
    assert_eq!(row, RowInsights::default());
}
