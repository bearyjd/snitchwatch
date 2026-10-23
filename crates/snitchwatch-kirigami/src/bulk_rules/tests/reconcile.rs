use snitchwatch_bridge::ws_messages::RuleCommandOutcome as O;

use super::fake::{plan_of, run_plan, Answer, Fake};
use crate::bulk_rules::{AbortReason, BatchState, NotSentWhy, RowStatus, Settings};

fn is_not_confirmed(s: &RowStatus) -> bool {
    matches!(s, RowStatus::NotConfirmed(_))
}

#[test]
fn a_timeout_that_was_applied_is_confirmed_by_content_and_not_resent() {
    let mut fake = Fake::with_answers(&[Answer::TimeoutApplied]);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::ConfirmedAfterNoAnswer);
    assert_eq!(progress.counts().added, 2);
    assert_eq!(fake.added(), ["a", "b"]);
    assert_eq!(fake.snapshots(), 2); // the first, and one to reconcile
}

#[test]
fn a_timeout_that_was_not_applied_is_reported_and_never_retried() {
    let mut fake = Fake::with_answers(&[Answer::Not(O::Timeout)]);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(is_not_confirmed(&progress.rows[0].status));
    assert_eq!(fake.added(), ["a", "b"], "a was sent once only");
    assert_eq!(progress.rows[1].status, RowStatus::Added);
}

#[test]
fn unsure_is_reconciled_the_same_way() {
    let unsure = || O::Unsure {
        reason: "maybe".into(),
    };
    let mut fake = Fake::with_answers(&[Answer::Applied(unsure()), Answer::Not(unsure())]);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::ConfirmedAfterNoAnswer);
    assert!(is_not_confirmed(&progress.rows[1].status));
    assert_eq!(fake.added(), ["a", "b"]);
}

#[test]
fn a_rule_of_that_name_with_other_content_after_a_timeout_is_left_alone() {
    let mut fake = Fake::with_answers(&[Answer::TimeoutOther]);
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::DifferentAfterNoAnswer);
    assert_eq!(progress.counts().left_alone, 1);
    assert_eq!(fake.added(), ["a"]);
}

#[test]
fn no_answer_at_all_is_a_timeout_after_the_bounded_wait() {
    let mut fake = Fake::with_answers(&[Answer::Silent]);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(is_not_confirmed(&progress.rows[0].status));
    assert!(fake.clock >= Settings::default().result_wait);
    assert_eq!(progress.rows[1].status, RowStatus::Added);
    assert_eq!(fake.added(), ["a", "b"]);
}

#[test]
fn an_unreadable_list_during_reconciliation_is_not_taken_as_absent() {
    let mut fake = Fake::with_answers(&[Answer::TimeoutApplied]);
    fake.snapshots_go_dark_after_add = true;
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    let RowStatus::NotConfirmed(why) = &progress.rows[0].status else {
        panic!("{:?}", progress.rows[0].status)
    };
    assert!(why.contains("not available"), "{why}");
    assert_eq!(fake.added(), ["a"]);
}

#[test]
fn three_timeouts_in_a_row_stop_the_batch() {
    let mut fake = Fake::with_answers(&[
        Answer::Not(O::Timeout),
        Answer::Not(O::Timeout),
        Answer::Not(O::Timeout),
    ]);
    let progress = run_plan(&plan_of(&["a", "b", "c", "d", "e"]), &mut fake);
    assert_eq!(fake.added(), ["a", "b", "c"]);
    assert_eq!(
        progress.state,
        BatchState::Aborted(AbortReason::RepeatedTimeouts(3))
    );
    assert!(is_not_confirmed(&progress.rows[2].status));
    assert_eq!(
        progress.rows[3].status,
        RowStatus::NotSent(NotSentWhy::Stopped)
    );
    assert_eq!(
        progress.rows[4].status,
        RowStatus::NotSent(NotSentWhy::Stopped)
    );
}

#[test]
fn timeouts_that_are_not_in_a_row_do_not_stop_it() {
    let mut fake = Fake::with_answers(&[
        Answer::Not(O::Timeout),
        Answer::Not(O::Timeout),
        Answer::Applied(O::Ok),
        Answer::Not(O::Timeout),
        Answer::Not(O::Timeout),
        Answer::Applied(O::Ok),
    ]);
    let progress = run_plan(&plan_of(&["a", "b", "c", "d", "e", "f"]), &mut fake);
    assert_eq!(progress.state, BatchState::Finished);
    assert_eq!(fake.added().len(), 6);
}

#[test]
fn a_list_gone_dark_after_a_timeout_stops_the_batch_before_the_next_rule() {
    let mut fake = Fake::with_answers(&[Answer::TimeoutApplied]);
    fake.snapshots_go_dark_after_add = true;
    let progress = run_plan(&plan_of(&["a", "b", "c"]), &mut fake);
    assert_eq!(fake.added(), ["a"], "b and c were never sent");
    assert!(matches!(
        progress.state,
        BatchState::Aborted(AbortReason::ListUnavailable(_))
    ));
    assert!(is_not_confirmed(&progress.rows[0].status));
    assert_eq!(
        progress.rows[1].status,
        RowStatus::NotSent(NotSentWhy::Stopped)
    );
    assert_eq!(
        progress.rows[2].status,
        RowStatus::NotSent(NotSentWhy::Stopped)
    );
}

#[test]
fn a_lag_settles_by_content_at_once_and_is_not_a_timeout() {
    // `a` is applied but its answer is lost to a lag; so is `b`'s, and `c`'s:
    // three lags are not three timeouts.
    let mut fake = Fake::with_answers(&[
        Answer::Lagged { applied: true },
        Answer::Lagged { applied: true },
        Answer::Lagged { applied: true },
        Answer::Applied(O::Ok),
    ]);
    let progress = run_plan(&plan_of(&["a", "b", "c", "d"]), &mut fake);
    assert_eq!(progress.state, BatchState::Finished);
    assert_eq!(
        progress.rows[..3]
            .iter()
            .map(|r| r.status.clone())
            .collect::<Vec<_>>(),
        vec![RowStatus::ConfirmedAfterNoAnswer; 3]
    );
    assert_eq!(progress.rows[3].status, RowStatus::Added);
    assert_eq!(fake.added(), ["a", "b", "c", "d"], "nothing is sent twice");
    assert!(
        fake.clock < Settings::default().result_wait,
        "a lag does not wait out the result"
    );
}

#[test]
fn a_lag_that_lost_an_unapplied_rule_is_reported_not_resent() {
    let mut fake = Fake::with_answers(&[Answer::Lagged { applied: false }]);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(is_not_confirmed(&progress.rows[0].status));
    assert_eq!(fake.added(), ["a", "b"]);
}
