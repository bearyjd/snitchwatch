use super::*;
use snitchwatch_bridge::ws_messages::RuleHitWire;

fn rule(name: &str, nolog: bool) -> Rule {
    Rule {
        name: name.to_string(),
        nolog,
        ..Default::default()
    }
}

fn storage(persistent: bool, reason: Option<&str>) -> StorageStatus {
    StorageStatus {
        persistent,
        reason: reason.map(str::to_string),
        unreadable: false,
    }
}

fn hits_message(since: Option<i64>, hits: &[(&str, u64, i64)]) -> ServerMessage {
    ServerMessage::RuleHits {
        since_unix_ms: since,
        lossy: false,
        last_gap_unix_ms: None,
        storage: storage(true, None),
        hits: hits
            .iter()
            .map(|(name, count, last)| RuleHitWire {
                name: (*name).to_string(),
                count: *count,
                last_hit_unix_ms: *last,
            })
            .collect(),
    }
}

fn counted(count: u64, last: i64) -> RowHits {
    RowHits::Counted {
        count,
        last_hit_unix_ms: last,
    }
}

fn info(view: &RuleHitsView) -> serde_json::Value {
    serde_json::from_str(&view.info_json()).unwrap()
}

#[test]
fn before_any_message_nothing_is_counted_or_claimed() {
    let view = RuleHitsView::default();
    assert_eq!(view.for_rule(&rule("a", false)), RowHits::Unavailable);
    assert_eq!(info(&view)["available"], false);
    assert_eq!(info(&view)["counting"], false);
}

#[test]
fn before_counting_starts_no_row_shows_a_count_but_the_storage_is_known() {
    let mut view = RuleHitsView::default();
    assert!(view.apply(&hits_message(None, &[])));
    assert_eq!(view.for_rule(&rule("a", false)), RowHits::Unavailable);
    let info = info(&view);
    assert_eq!(info["available"], true);
    assert_eq!(info["counting"], false);
    assert_eq!(info["persistent"], true);
}

#[test]
fn rows_show_their_count_and_a_rule_without_hits_is_zero_not_missing() {
    let mut view = RuleHitsView::default();
    view.apply(&hits_message(Some(1_000), &[("a", 3, 5_000)]));
    assert_eq!(view.for_rule(&rule("a", false)), counted(3, 5_000));
    assert_eq!(view.for_rule(&rule("b", false)), counted(0, 0));
}

/// The daemon never reports an event for a `nolog` rule, so a "0" there would
/// say the rule is unused when it only says nothing.
#[test]
fn a_nolog_rule_is_never_shown_as_zero() {
    let mut view = RuleHitsView::default();
    view.apply(&hits_message(Some(1_000), &[("quiet", 7, 5_000)]));
    let shown = view.for_rule(&rule("quiet", true));
    assert_eq!(shown, RowHits::NotCounted);
    assert_eq!(
        RowHits::NotCounted.note(),
        "Not counted: this rule doesn't log"
    );
    assert_eq!(counted(1, 1).note(), "");
    assert_eq!(RowHits::Unavailable.note(), "");
}

#[test]
fn a_new_message_replaces_the_counts() {
    let mut view = RuleHitsView::default();
    view.apply(&hits_message(Some(1_000), &[("a", 3, 5_000)]));
    view.apply(&hits_message(Some(1_000), &[("b", 1, 6_000)]));
    assert_eq!(view.for_rule(&rule("a", false)), counted(0, 0));
    assert_eq!(view.for_rule(&rule("b", false)), counted(1, 6_000));
}

#[test]
fn counts_belong_to_the_session_that_sent_them() {
    let mut view = RuleHitsView::default();
    assert!(!view.note_session(1), "nothing to forget");
    view.apply(&hits_message(Some(1_000), &[("a", 3, 5_000)]));
    assert!(!view.note_session(1), "same session");
    assert_eq!(view.for_rule(&rule("a", false)), counted(3, 5_000));
    assert!(view.note_session(2), "a new bridge session");
    assert_eq!(view.for_rule(&rule("a", false)), RowHits::Unavailable);
    assert_eq!(info(&view)["available"], false);
}

#[test]
fn other_messages_change_nothing() {
    let mut view = RuleHitsView::default();
    assert!(!view.apply(&ServerMessage::SetRules { rules: vec![] }));
}

#[test]
fn the_summary_carries_what_makes_the_counts_approximate() {
    let mut view = RuleHitsView::default();
    view.apply(&ServerMessage::RuleHits {
        since_unix_ms: Some(1_800_000_000_000),
        lossy: true,
        last_gap_unix_ms: Some(1_800_000_100_000),
        storage: storage(false, Some("state directory /x: gone")),
        hits: vec![],
    });
    let info = info(&view);
    assert_eq!(info["counting"], true);
    assert_eq!(info["sinceMs"], 1_800_000_000_000_i64);
    assert_eq!(info["lossy"], true);
    assert_eq!(info["lastGapMs"], 1_800_000_100_000_i64);
    assert_eq!(info["persistent"], false);
    assert_eq!(info["storageReason"], "state directory /x: gone");
}

#[test]
fn a_huge_count_is_clamped_for_the_model() {
    assert_eq!(counted(u64::MAX, 1).count_for_model(), i32::MAX);
    assert_eq!(counted(7, 1).count_for_model(), 7);
    assert_eq!(RowHits::Unavailable.count_for_model(), 0);
}
