use super::*;

fn set() -> ReviewSession {
    session(vec![
        narrow("alpha"),
        narrow("beta"),
        tmp("gamma"),
        dual("python3"),
        program("daemon", None, &[("a.example", 3)]),
    ])
}

#[test]
fn nothing_is_ticked_when_a_session_is_built() {
    let s = set();
    assert_eq!(s.proposals().len(), 5);
    assert_eq!(s.tick_count(), 0);
    assert_eq!(s.ticks_json(), "{}");
}

#[test]
fn select_suggested_ticks_exactly_the_suggested_rows_with_their_host_rules() {
    let s = set();
    let suggested: Vec<String> = s
        .proposals()
        .iter()
        .filter(|p| p.selected_by_suggest)
        .map(|p| p.key.clone())
        .collect();
    assert_eq!(suggested.len(), 2, "alpha and beta only");
    let ticked = s.select_suggested();
    let mut got: Vec<String> = ticked.ticks().keys().cloned().collect();
    let mut want = suggested;
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert!(ticked.ticks().values().all(|k| *k == K::AllowHosts));
    // The original is unchanged.
    assert_eq!(s.tick_count(), 0);
}

#[test]
fn select_suggested_never_replaces_a_choice_the_owner_made() {
    let s = set();
    let alpha = key_of(&s, "/usr/bin/alpha");
    let owner = s.tick(&alpha, Some(K::Block)).unwrap().select_suggested();
    assert_eq!(owner.ticks().get(&alpha), Some(&K::Block));
    assert_eq!(owner.tick_count(), 2, "beta was added, alpha kept");
}

#[test]
fn only_a_choice_the_row_offers_can_be_ticked() {
    let s = set();
    let nameless = s
        .proposals()
        .iter()
        .find(|p| p.key.starts_with("name:"))
        .unwrap()
        .key
        .clone();
    for kind in [K::AllowAny, K::AllowHosts, K::Block, K::NonSelectable] {
        assert_eq!(
            s.tick(&nameless, Some(kind)).unwrap_err(),
            Refusal::NotOffered,
            "{kind:?}"
        );
    }
    assert_eq!(
        s.tick("path:/nowhere", Some(K::AllowAny)).unwrap_err(),
        Refusal::NotOffered
    );
    // A directory rule is offered only for a versioned binary.
    let alpha = key_of(&s, "/usr/bin/alpha");
    assert_eq!(
        s.tick(&alpha, Some(K::DirectoryRule)).unwrap_err(),
        Refusal::NotOffered
    );
}

#[test]
fn a_row_has_one_tick_and_untick_removes_it() {
    let s = set();
    let alpha = key_of(&s, "/usr/bin/alpha");
    let a = s.tick(&alpha, Some(K::AllowAny)).unwrap();
    let b = a.tick(&alpha, Some(K::Block)).unwrap();
    assert_eq!(b.tick_count(), 1);
    assert_eq!(b.ticks().get(&alpha), Some(&K::Block));
    assert_eq!(b.tick(&alpha, None).unwrap().tick_count(), 0);
    assert_eq!(b.select_suggested().untick_all().tick_count(), 0);
}

#[test]
fn a_tick_change_drops_the_frozen_plan() {
    let s = set();
    let alpha = key_of(&s, "/usr/bin/alpha");
    let frozen = s.tick(&alpha, Some(K::AllowAny)).unwrap().freeze().unwrap();
    assert!(frozen.authorize().is_ok());
    let changed = frozen.tick(&alpha, Some(K::AllowHosts)).unwrap();
    assert_eq!(changed.authorize().unwrap_err(), Refusal::NotFrozen);
    assert_eq!(changed.summary_json(), "null");
}

#[test]
fn a_rule_the_owner_already_has_is_not_offered() {
    use crate::learning_review::{rules_for, Choice};
    let program = narrow("alpha");
    let mut aggregate = Aggregate::default();
    aggregate.absorb(program.clone());
    let any = rules_for(&Choice::AllowAny, &program.normalized()).unwrap();
    let rule: Rule =
        serde_json::from_value(crate::learning_review::RulePayload::to_wire(&any[0])).unwrap();
    let with_rule = ReviewSession::new(&aggregate, &[rule], true);
    let row = &with_rule.proposals()[0];
    assert!(row.choice(K::AllowAny).is_none());
    assert_eq!(row.already_present.len(), 1);
    assert!(with_rule
        .rows_json()
        .contains("\"alreadyPresent\":[\"allow-any\"]"));
}

#[test]
fn a_frozen_summary_is_dropped_by_unfreeze_so_a_reopened_page_starts_clean() {
    let s = set();
    let alpha = key_of(&s, "/usr/bin/alpha");
    let frozen = s.tick(&alpha, Some(K::AllowAny)).unwrap().freeze().unwrap();
    assert!(frozen.summary().is_some());
    let reopened = frozen.unfreeze();
    assert_eq!(reopened.summary_json(), "null");
    assert_eq!(reopened.authorize().unwrap_err(), Refusal::NotFrozen);
    assert_eq!(reopened.tick_count(), 1, "the ticks stay");
}
