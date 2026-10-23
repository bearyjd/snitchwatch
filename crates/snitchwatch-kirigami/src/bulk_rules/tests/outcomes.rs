use snitchwatch_bridge::rule_policy::RuleProblem;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome as O;

use super::fake::{plan_of, run_plan, Answer, Fake};
use crate::bulk_rules::{run, AbortReason, BatchState, NotSentWhy, RowStatus, RuleSink, Settings};

fn kinds(p: &crate::bulk_rules::Progress) -> Vec<RowStatus> {
    p.rows.iter().map(|r| r.status.clone()).collect()
}

#[test]
fn rules_go_one_at_a_time_in_order_after_one_snapshot() {
    let plan = plan_of(&["a", "b", "c"]);
    let mut fake = Fake::new();
    let progress = run_plan(&plan, &mut fake);
    assert_eq!(fake.log, ["snapshot", "add:a", "add:b", "add:c"]);
    assert_eq!(kinds(&progress), vec![RowStatus::Added; 3]);
    assert_eq!(progress.state, BatchState::Finished);
    assert_eq!(progress.counts().added, 3);
}

#[test]
fn a_result_for_another_request_is_not_taken_as_the_answer() {
    let plan = plan_of(&["a", "b"]);
    let mut fake = Fake::with_answers(&[Answer::Applied(O::Ok), Answer::Applied(O::Ok)]);
    fake.stray_results = true;
    let progress = run_plan(&plan, &mut fake);
    assert_eq!(kinds(&progress), vec![RowStatus::Added; 2]);
}

#[test]
fn every_request_id_is_new_and_valid() {
    struct Ids(Vec<String>, Fake);
    impl RuleSink for Ids {
        fn new_request_id(&mut self) -> String {
            self.1.new_request_id()
        }
        fn send_add(
            &mut self,
            id: &str,
            r: &crate::learning_review::RulePayload,
        ) -> Result<(), crate::bulk_rules::SendError> {
            self.0.push(id.to_owned());
            self.1.send_add(id, r)
        }
        fn request_snapshot(&mut self) -> Result<(), crate::bulk_rules::SendError> {
            self.1.request_snapshot()
        }
        fn next_message(
            &mut self,
            w: std::time::Duration,
        ) -> Result<crate::bulk_rules::Incoming, crate::bulk_rules::SessionLost> {
            self.1.next_message(w)
        }
        fn sleep(&mut self, d: std::time::Duration) {
            self.1.sleep(d)
        }
        fn now(&self) -> std::time::Duration {
            self.1.now()
        }
        fn cancelled(&self) -> bool {
            false
        }
    }
    let mut sink = Ids(vec![], Fake::new());
    run(
        &plan_of(&["a", "b"]),
        &mut sink,
        &Settings::default(),
        &mut |_| {},
    );
    assert_eq!(sink.0.len(), 2);
    assert_ne!(sink.0[0], sink.0[1]);
}

#[test]
fn ok_with_note_refused_and_rejected_are_shown_and_the_rest_continue() {
    let problems = vec![RuleProblem {
        path: "operator".into(),
        reason: "matches everything".into(),
    }];
    let mut fake = Fake::with_answers(&[
        Answer::Applied(O::OkWithNote {
            note: "old file kept".into(),
        }),
        Answer::Not(O::Refused { problems }),
        Answer::Not(O::Rejected {
            reason: "daemon said no".into(),
        }),
        Answer::Applied(O::Ok),
    ]);
    let progress = run_plan(&plan_of(&["a", "b", "c", "d"]), &mut fake);
    assert_eq!(
        kinds(&progress),
        vec![
            RowStatus::AddedWithNote("old file kept".into()),
            RowStatus::Refused(vec!["operator: matches everything".into()]),
            RowStatus::Rejected("daemon said no".into()),
            RowStatus::Added,
        ]
    );
    assert_eq!(fake.added(), ["a", "b", "c", "d"]);
    let c = progress.counts();
    assert_eq!((c.added, c.refused, c.rejected), (2, 1, 1));
    assert_eq!(progress.state, BatchState::Finished);
}

#[test]
fn no_daemon_stops_the_whole_batch() {
    let mut fake = Fake::with_answers(&[Answer::Applied(O::Ok), Answer::Not(O::NoDaemon)]);
    let progress = run_plan(&plan_of(&["a", "b", "c", "d"]), &mut fake);
    assert_eq!(fake.added(), ["a", "b"]);
    assert_eq!(
        kinds(&progress),
        vec![
            RowStatus::Added,
            RowStatus::NotSent(NotSentWhy::Stopped),
            RowStatus::NotSent(NotSentWhy::Stopped),
            RowStatus::NotSent(NotSentWhy::Stopped),
        ]
    );
    assert_eq!(progress.state, BatchState::Aborted(AbortReason::NoDaemon));
}

#[test]
fn the_session_ending_stops_the_batch() {
    let mut fake = Fake::new();
    fake.lose_session_on_next_wait = true;
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(
        progress.state,
        BatchState::Aborted(AbortReason::SessionLost)
    );
    assert_eq!(fake.added(), Vec::<String>::new());
}

#[test]
fn the_observer_sees_each_row_sending_then_settled() {
    let plan = plan_of(&["a", "b"]);
    let mut fake = Fake::new();
    let mut seen = vec![];
    let done = run(&plan, &mut fake, &Settings::default(), &mut |p| {
        seen.push(p.clone())
    });
    assert!(seen.iter().any(|p| p.rows[0].status == RowStatus::Sending));
    assert!(seen
        .iter()
        .any(|p| p.rows[0].status == RowStatus::Added && p.rows[1].status == RowStatus::Pending));
    assert_eq!(seen.last(), Some(&done));
    assert_eq!(done.summary(), "2 added");
}

#[test]
fn the_summary_names_a_stop_and_the_counts() {
    let mut fake = Fake::with_answers(&[Answer::Applied(O::Ok), Answer::Not(O::NoDaemon)]);
    let p = run_plan(&plan_of(&["a", "b", "c"]), &mut fake);
    assert_eq!(
        p.summary(),
        "Stopped. The firewall service is not connected. 1 added, 2 not sent"
    );
}
