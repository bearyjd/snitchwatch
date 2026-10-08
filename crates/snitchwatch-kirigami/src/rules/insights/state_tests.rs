use std::collections::BTreeMap;

use super::shadow::{Analysis, Finding, FindingKind, MAX_ANALYZED_RULES};
use super::state::{AnalysisState, Phase};

fn done(names: &[(&str, FindingKind)]) -> Analysis {
    Analysis::Done {
        findings: names
            .iter()
            .map(|(name, kind)| {
                (
                    (*name).to_string(),
                    Finding {
                        kind: *kind,
                        by: "by".into(),
                        by_display: "By".into(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    }
}

fn json(state: &AnalysisState) -> serde_json::Value {
    serde_json::from_str(&state.summary_json()).unwrap()
}

#[test]
fn an_analysis_shows_only_for_the_rule_list_it_was_computed_from() {
    let mut state = AnalysisState::default();
    assert_eq!(state.phase(), Phase::Idle);
    let generation = state.start().expect("starts").generation;
    assert_eq!(state.phase(), Phase::Running);
    state.finish(generation, done(&[("a", FindingKind::NeverDecides)]));
    assert_eq!(state.phase(), Phase::Done);
    assert!(state.finding("a").is_some());
    assert!(state.finding("b").is_none());

    state.rules_changed();
    assert_eq!(state.phase(), Phase::Stale);
    assert!(state.finding("a").is_none(), "an old claim is not shown");
}

#[test]
fn a_rule_list_that_changes_while_the_analysis_runs_makes_its_result_stale() {
    let mut state = AnalysisState::default();
    let generation = state.start().unwrap().generation;
    state.rules_changed();
    assert_eq!(state.phase(), Phase::Stale);
    state.finish(generation, done(&[("a", FindingKind::NeverDecides)]));
    assert_eq!(state.phase(), Phase::Stale, "the old result is dropped");
    assert!(state.finding("a").is_none());
    // And a new request analyses the new list.
    let next = state.start().unwrap().generation;
    assert_ne!(next, generation);
    state.finish(next, done(&[("b", FindingKind::NeverDecides)]));
    assert_eq!(state.phase(), Phase::Done);
    assert!(state.finding("b").is_some());
}

/// The first worker is still computing when the user asks again.
#[test]
fn a_result_from_an_older_run_never_lands_on_a_newer_one() {
    let mut state = AnalysisState::default();
    let old = state.start().unwrap().generation;
    state.rules_changed();
    let new = state.start().unwrap().generation;
    assert_ne!(old, new);

    state.finish(old, done(&[("old", FindingKind::NeverDecides)]));
    assert_eq!(
        state.phase(),
        Phase::Running,
        "still waiting for the new run"
    );
    assert!(state.finding("old").is_none());

    state.finish(new, done(&[("new", FindingKind::NeverDecides)]));
    assert_eq!(state.phase(), Phase::Done);
    assert!(state.finding("new").is_some());
    assert!(state.finding("old").is_none());
}

#[test]
fn nothing_changes_for_a_rule_list_change_before_anyone_asked() {
    let mut state = AnalysisState::default();
    state.rules_changed();
    assert_eq!(state.phase(), Phase::Idle);
}

#[test]
fn a_second_request_while_running_is_refused() {
    let mut state = AnalysisState::default();
    assert!(state.start().is_some());
    assert!(state.start().is_none());
}

#[test]
fn too_many_rules_is_reported_not_analysed() {
    let mut state = AnalysisState::default();
    let generation = state.start().unwrap().generation;
    state.finish(
        generation,
        Analysis::TooMany {
            enabled: 2500,
            limit: MAX_ANALYZED_RULES,
        },
    );
    assert_eq!(state.phase(), Phase::TooMany);
    let summary = json(&state);
    assert_eq!(summary["state"], "tooMany");
    assert_eq!(summary["enabled"], 2500);
    assert_eq!(summary["limit"], MAX_ANALYZED_RULES);
}

#[test]
fn the_summary_counts_each_kind() {
    let mut state = AnalysisState::default();
    let generation = state.start().unwrap().generation;
    state.finish(
        generation,
        done(&[
            ("a", FindingKind::NeverDecides),
            ("b", FindingKind::NeverDecides),
            ("c", FindingKind::MayBeShadowed),
        ]),
    );
    let summary = json(&state);
    assert_eq!(summary["state"], "done");
    assert_eq!(summary["neverDecides"], 2);
    assert_eq!(summary["mayBeShadowed"], 1);
}

#[test]
fn a_rule_list_change_tells_the_run_in_flight_to_stop() {
    use std::sync::atomic::Ordering;
    let mut state = AnalysisState::default();
    let first = state.start().unwrap();
    assert!(!first.cancel.load(Ordering::Relaxed));
    state.rules_changed();
    assert!(first.cancel.load(Ordering::Relaxed), "the old run is told");
    // The new run starts with a fresh flag.
    let second = state.start().unwrap();
    assert!(!second.cancel.load(Ordering::Relaxed));
    state.rules_changed();
    assert!(second.cancel.load(Ordering::Relaxed));
    // A finished run isn't cancelled by a later change (nothing to stop).
    let third = state.start().unwrap();
    state.finish(third.generation, done(&[]));
    state.rules_changed();
    assert!(!third.cancel.load(Ordering::Relaxed));
}

#[test]
fn a_finish_nobody_is_waiting_for_changes_nothing() {
    let mut state = AnalysisState::default();
    state.finish(0, done(&[("a", FindingKind::NeverDecides)]));
    assert_eq!(state.phase(), Phase::Idle);
    assert!(state.finding("a").is_none());

    // And not twice for the same run.
    let generation = state.start().unwrap().generation;
    state.finish(generation, done(&[("a", FindingKind::NeverDecides)]));
    state.finish(generation, done(&[("b", FindingKind::NeverDecides)]));
    assert!(state.finding("a").is_some());
    assert!(state.finding("b").is_none());
}
