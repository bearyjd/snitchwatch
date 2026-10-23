use serde_json::{json, Value};

use super::fake::{payload_with, plan_of, planned, run_plan, Fake};
use crate::bulk_rules::{run, AbortReason, BatchState, BulkPlan, NotSentWhy, RowStatus, Settings};

fn existing(name: &str, path: &str) -> Value {
    payload_with(name, path).to_wire()
}

#[test]
fn an_identical_rule_is_skipped_and_not_sent() {
    let mut fake = Fake::new();
    fake.rules = vec![existing("a", "/usr/bin/a")];
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(fake.added(), ["b"]);
    assert_eq!(progress.rows[0].status, RowStatus::AlreadyPresent);
    assert_eq!(progress.rows[1].status, RowStatus::Added);
    assert_eq!(progress.counts().already_present, 1);
}

#[test]
fn an_edited_rule_is_not_overwritten_and_the_rest_continue() {
    let mut fake = Fake::new();
    fake.rules = vec![existing("a", "/usr/bin/other")];
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert_eq!(fake.added(), ["b"]);
    assert_eq!(progress.rows[0].status, RowStatus::ChangedByOwner);
    assert_eq!(progress.rows[0].status.text(), "Changed by you, left alone");
    assert_eq!(progress.counts().left_alone, 1);
    assert_eq!(progress.state, BatchState::Finished);
}

#[test]
fn each_field_that_decides_traffic_counts_as_a_change() {
    for (field, value) in [
        ("action", json!("deny")),
        ("duration", json!("5m")),
        ("enabled", json!(false)),
        ("precedence", json!(true)),
        ("nolog", json!(true)),
    ] {
        let mut wire = existing("a", "/usr/bin/a");
        wire[field] = value;
        let mut fake = Fake::new();
        fake.rules = vec![wire];
        let progress = run_plan(&plan_of(&["a"]), &mut fake);
        assert_eq!(
            progress.rows[0].status,
            RowStatus::ChangedByOwner,
            "{field}"
        );
        assert!(fake.added().is_empty(), "{field}");
    }
}

#[test]
fn description_and_display_fields_are_not_content() {
    let mut wire = existing("a", "/usr/bin/a");
    wire["description"] = json!("edited text");
    wire["created"] = json!(12345);
    wire["displayName"] = json!("shown");
    let mut fake = Fake::new();
    fake.rules = vec![wire];
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::AlreadyPresent);
}

#[test]
fn the_comparison_normalises_the_operator_tree() {
    // A host rule: an AND list; the daemon may report it in another order,
    // with `sensitive` left out and an upper-case kind.
    let leaf = |operand: &str, data: &str| json!({"type": "simple", "operand": operand, "data": data, "sensitive": false});
    let planned_rule = {
        let mut p = payload_with("h", "x");
        p.rule.operator = Some(snitchwatch_proto::protocol::Operator {
            r#type: "list".into(),
            list: vec![
                snitchwatch_proto::protocol::Operator {
                    r#type: "simple".into(),
                    operand: "process.path".into(),
                    data: "/usr/bin/x".into(),
                    ..Default::default()
                },
                snitchwatch_proto::protocol::Operator {
                    r#type: "simple".into(),
                    operand: "dest.host".into(),
                    data: "example.org".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        p
    };
    let plan = BulkPlan::new(vec![crate::bulk_rules::PlannedRule {
        payload: planned_rule.clone(),
        label: "h".into(),
    }])
    .unwrap();
    let mut wire = planned_rule.to_wire();
    wire["operator"] = json!({
        "type": "LIST",
        "operands": [
            {"type": "simple", "operand": " dest.host ", "data": "example.org"},
            leaf("process.path", "/usr/bin/x"),
        ],
    });
    let mut fake = Fake::new();
    fake.rules = vec![wire.clone()];
    let progress = run_plan(&plan, &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::AlreadyPresent);
    // Case in the data is content.
    wire["operator"]["operands"][0]["data"] = json!("Example.org");
    let mut fake = Fake::new();
    fake.rules = vec![wire.clone()];
    assert_eq!(
        run_plan(&plan, &mut fake).rows[0].status,
        RowStatus::ChangedByOwner
    );
    // An unreadable tree is never "the same".
    wire["operator"] = json!({"type": "simple", "data": 5});
    let mut fake = Fake::new();
    fake.rules = vec![wire];
    assert_eq!(
        run_plan(&plan, &mut fake).rows[0].status,
        RowStatus::ChangedByOwner
    );
}

#[test]
fn an_unknown_list_aborts_and_an_empty_unknown_list_is_not_no_such_rule() {
    let mut fake = Fake::new();
    fake.listed = false; // the bridge sends an empty list while it is Unknown
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    assert!(matches!(
        progress.state,
        BatchState::Aborted(AbortReason::ListUnavailable(_))
    ));
    assert!(fake.added().is_empty());
    assert_eq!(
        progress
            .rows
            .iter()
            .map(|r| r.status.clone())
            .collect::<Vec<_>>(),
        vec![RowStatus::NotSent(NotSentWhy::Stopped); 2]
    );
    assert!(progress.summary().contains("rules list is not available"));
}

#[test]
fn a_genuinely_empty_known_list_lets_the_batch_run() {
    let mut fake = Fake::new();
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(fake.added(), ["a"]);
    assert_eq!(progress.state, BatchState::Finished);
}

#[test]
fn no_list_at_all_aborts_after_the_bounded_wait() {
    let mut fake = Fake::new();
    fake.answers_snapshot = false;
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert!(matches!(
        progress.state,
        BatchState::Aborted(AbortReason::ListUnavailable(_))
    ));
    assert!(fake.clock >= Settings::default().list_wait);
    assert!(fake.clock < Settings::default().list_wait * 2);
    assert!(fake.added().is_empty());
}

#[test]
fn a_list_over_the_read_limit_aborts() {
    let mut fake = Fake::new();
    fake.over_limit_total = Some(20_000);
    fake.listed = false;
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert!(matches!(
        progress.state,
        BatchState::Aborted(AbortReason::ListUnavailable(_))
    ));
}

#[test]
fn a_snapshot_that_cannot_be_queued_aborts() {
    use crate::bulk_rules::SendError;
    let mut fake = Fake::new();
    fake.snapshot_error = Some(SendError::NotConnected);
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(
        progress.state,
        BatchState::Aborted(AbortReason::NotConnected)
    );
}

#[test]
fn rules_the_list_leaves_out_stop_an_absent_name_from_being_assumed_free() {
    let mut fake = Fake::new();
    fake.too_large = 1;
    fake.rules = vec![existing("a", "/usr/bin/a")];
    let progress = run_plan(&plan_of(&["a", "b"]), &mut fake);
    // `a` is found and identical; `b` is absent but may be a left-out rule.
    assert_eq!(progress.rows[0].status, RowStatus::AlreadyPresent);
    assert_eq!(progress.rows[1].status, RowStatus::NotChecked);
    assert!(fake.added().is_empty());
    assert_eq!(progress.counts().unconfirmed, 1);
}

#[test]
fn a_listed_entry_that_cannot_be_read_counts_as_left_out() {
    let mut fake = Fake::new();
    fake.rules = vec![json!({"name": 5, "operator": "garbage"})];
    let progress = run_plan(&plan_of(&["a"]), &mut fake);
    assert_eq!(progress.rows[0].status, RowStatus::NotChecked);
    assert!(fake.added().is_empty());
}

#[test]
fn a_name_twice_in_a_plan_is_planned_once() {
    let plan = BulkPlan::new(vec![planned("a"), planned("b"), planned("a")]).unwrap();
    assert_eq!(plan.rules().len(), 2);
    let mut fake = Fake::new();
    run(&plan, &mut fake, &Settings::default(), &mut |_| {});
    assert_eq!(fake.added(), ["a", "b"]);
}

#[test]
fn a_list_withdrawn_mid_batch_stops_the_next_rule_from_being_sent() {
    // The bridge withdraws the list (an empty `SetRules`, then `listed:
    // false`) after `a`. An owner-edited `c` would look "absent": it must
    // not be sent over.
    let mut fake = Fake::new();
    fake.withdraw_after_adds = Some(1);
    let progress = run_plan(&plan_of(&["a", "b", "c"]), &mut fake);
    assert_eq!(fake.added(), ["a"]);
    assert_eq!(progress.rows[0].status, RowStatus::Added);
    assert_eq!(
        progress.rows[1].status,
        RowStatus::NotSent(NotSentWhy::Stopped)
    );
    assert!(matches!(
        progress.state,
        BatchState::Aborted(AbortReason::ListUnavailable(_))
    ));
}

#[test]
fn a_list_that_comes_back_after_a_withdrawal_lets_the_batch_go_on() {
    // Withdrawn, but the answer to the new snapshot is a real list.
    let mut fake = Fake::new();
    fake.withdraw_after_adds = Some(1);
    let plan = plan_of(&["a", "b"]);
    struct Heal(Fake);
    impl crate::bulk_rules::RuleSink for Heal {
        fn new_request_id(&mut self) -> String {
            self.0.new_request_id()
        }
        fn send_add(
            &mut self,
            i: &str,
            r: &crate::learning_review::RulePayload,
        ) -> Result<(), crate::bulk_rules::SendError> {
            self.0.send_add(i, r)
        }
        fn request_snapshot(&mut self) -> Result<(), crate::bulk_rules::SendError> {
            self.0.listed = true;
            self.0.request_snapshot()
        }
        fn next_message(
            &mut self,
            w: std::time::Duration,
        ) -> Result<crate::bulk_rules::Incoming, crate::bulk_rules::SessionLost> {
            self.0.next_message(w)
        }
        fn sleep(&mut self, d: std::time::Duration) {
            self.0.sleep(d)
        }
        fn now(&self) -> std::time::Duration {
            self.0.now()
        }
        fn cancelled(&self) -> bool {
            false
        }
    }
    let mut sink = Heal(fake);
    let progress = run(&plan, &mut sink, &Settings::default(), &mut |_| {});
    assert_eq!(progress.state, BatchState::Finished);
    assert_eq!(sink.0.added(), ["a", "b"]);
    assert_eq!(sink.0.snapshots(), 2, "a fresh list was asked for first");
}

#[test]
fn a_list_that_may_not_match_the_service_does_not_clear_a_name() {
    for (left_on_disk, count_mismatch) in [(1, false), (0, true)] {
        let mut fake = Fake::new();
        fake.left_on_disk = left_on_disk;
        fake.count_mismatch = count_mismatch;
        let progress = run_plan(&plan_of(&["a"]), &mut fake);
        assert_eq!(progress.rows[0].status, RowStatus::NotChecked);
        assert!(fake.added().is_empty());
    }
}

#[test]
fn a_repeated_name_is_reported_as_dropped() {
    let plan = BulkPlan::new(vec![planned("a"), planned("a"), planned("a")]).unwrap();
    assert_eq!(plan.dropped(), ["a", "a"]);
    let mut fake = Fake::new();
    let progress = run(&plan, &mut fake, &Settings::default(), &mut |_| {});
    assert_eq!(progress.dropped, ["a", "a"]);
    assert_eq!(progress.summary(), "1 added, 2 repeated names planned once");
    assert!(BulkPlan::new(vec![planned("a"), planned("b")])
        .unwrap()
        .dropped()
        .is_empty());
}
