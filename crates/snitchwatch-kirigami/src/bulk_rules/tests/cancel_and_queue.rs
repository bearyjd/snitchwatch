use std::time::Duration;

use snitchwatch_bridge::ws_messages::RuleCommandOutcome as O;

use super::fake::{plan_of, run_plan, Answer, Fake};
use crate::bulk_rules::{run, AbortReason, BatchState, NotSentWhy, RowStatus, SendError, Settings};

#[test]
fn a_full_queue_is_retried_with_a_growing_wait() {
    let mut fake = Fake::new();
    fake.send_errors = [SendError::Full, SendError::Full, SendError::Full].into();
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::Added);
    assert_eq!(
        fake.sleeps,
        [
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(200)
        ]
    );
    assert_eq!(fake.added(), ["a"], "sent once, after the queue had room");
}

#[test]
fn the_wait_is_capped() {
    let mut fake = Fake::new();
    fake.send_errors = std::iter::repeat_n(SendError::Full, 7).collect();
    run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(fake.sleeps.last(), Some(&Duration::from_secs(1)));
    assert!(fake.sleeps.iter().all(|d| *d <= Duration::from_secs(1)));
}

#[test]
fn a_queue_that_stays_full_stops_the_batch() {
    let mut fake = Fake::new();
    fake.send_errors = std::iter::repeat_n(SendError::Full, 100).collect();
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(progress.state, BatchState::Aborted(AbortReason::QueueBusy));
    assert_eq!(fake.sleeps.len(), 7, "8 tries, a wait between each");
    assert_eq!(
        progress.rows[0].status,
        RowStatus::NotSent(NotSentWhy::Stopped)
    );
    assert!(fake.added().is_empty());
}

#[test]
fn no_connection_stops_the_batch_without_waiting() {
    let mut fake = Fake::new();
    fake.send_errors = [SendError::NotConnected].into();
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(
        progress.state,
        BatchState::Aborted(AbortReason::NotConnected)
    );
    assert!(fake.sleeps.is_empty());
    assert_eq!(progress.counts().not_sent, 2);
}

#[test]
fn cancel_reports_the_rest_as_not_sent() {
    let mut fake = Fake::with_answers(&[Answer::Applied(O::Ok)]);
    fake.cancel_after_adds = Some(1);
    let progress = run_plan(&plan_of(&["a", "b", "c"]), &mut fake);
    assert_eq!(fake.added(), ["a"]);
    assert_eq!(progress.state, BatchState::Cancelled);
    assert_eq!(progress.rows[0].status, RowStatus::Added);
    assert_eq!(
        progress.rows[1].status,
        RowStatus::NotSent(NotSentWhy::Cancelled)
    );
    assert_eq!(progress.rows[2].status.text(), "Not sent: cancelled");
    assert_eq!(progress.summary(), "Cancelled. 1 added, 2 not sent");
}

#[test]
fn cancel_before_the_start_sends_nothing_not_even_a_snapshot() {
    let mut fake = Fake::new();
    fake.cancel_after_adds = Some(0);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(fake.log.is_empty());
    assert_eq!(progress.state, BatchState::Cancelled);
    assert_eq!(progress.counts().not_sent, 2);
}

#[test]
fn settings_can_make_the_timeout_limit_smaller() {
    let mut fake = Fake::with_answers(&[Answer::Not(O::Timeout), Answer::Not(O::Timeout)]);
    let settings = Settings {
        max_consecutive_timeouts: 2,
        ..Settings::default()
    };
    let progress = run(
        &plan_of(&["a", "b", "c"]),
        &mut fake,
        &settings,
        &mut |_| {},
    );
    assert_eq!(
        progress.state,
        BatchState::Aborted(AbortReason::RepeatedTimeouts(2))
    );
}

#[test]
fn a_rule_cancelled_while_waiting_for_its_answer_is_unconfirmed() {
    let mut fake = Fake::with_answers(&[Answer::Silent]);
    fake.cancel_after_adds = Some(1);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(matches!(
        progress.rows[0].status,
        RowStatus::NotConfirmed(_)
    ));
    assert_eq!(
        progress.rows[1].status,
        RowStatus::NotSent(NotSentWhy::Cancelled)
    );
    assert_eq!(progress.state, BatchState::Cancelled);
    assert!(
        fake.clock < Settings::default().result_wait,
        "the wait did not run out"
    );
}

#[test]
fn a_list_withdrawn_during_the_backoff_stops_the_retry() {
    let mut fake = Fake::new();
    fake.send_errors = [SendError::Full].into();
    fake.withdraw_on_sleep = true;
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(
        fake.added().is_empty(),
        "the rule was not sent: {:?}",
        fake.log
    );
    assert!(matches!(
        progress.state,
        BatchState::Aborted(AbortReason::ListUnavailable(_))
    ));
}

#[test]
fn a_cancel_after_the_list_check_stops_the_send() {
    let mut fake = Fake::new();
    fake.cancel_after_snapshots = Some(1);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(fake.added().is_empty());
    assert_eq!(progress.state, BatchState::Cancelled);
    assert_eq!(progress.counts().not_sent, 2);
}

#[test]
fn the_bridges_real_answer_pair_costs_no_extra_snapshot() {
    let mut fake = Fake::new();
    fake.list_after_add = true;
    let progress = run_plan(&plan_of(&["a", "b", "c"]), &mut fake);
    assert_eq!(progress.state, BatchState::Finished);
    assert_eq!(fake.added(), ["a", "b", "c"]);
    assert!(fake.snapshots() <= 1 + 3, "{} snapshots", fake.snapshots());
}

#[test]
fn a_cancel_between_the_look_up_and_the_send_stops_the_send() {
    let mut fake = Fake::new();
    // The second rule's list check is the second drain; nothing else looks
    // at the flag between it and the send.
    fake.cancel_after_drains = Some(2);
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(fake.added(), ["a"]);
    assert_eq!(progress.state, BatchState::Cancelled);
}
