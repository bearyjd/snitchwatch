use super::fake::{payload_with, planned};
use crate::bulk_rules::{BulkPlan, PlanError, PlannedRule};
use crate::learning_review::{rules_for, Choice, ALLOW_NAME_PREFIX, BLOCK_NAME_PREFIX};

fn named(name: &str) -> PlannedRule {
    PlannedRule {
        payload: payload_with(name, "/usr/bin/x"),
        label: "x".into(),
    }
}

#[test]
fn the_reserved_prefix_is_never_used() {
    for name in [
        "snitchwatch-allow-github.com-443-0123abcd",
        "snitchwatch-default-x",
        "z00-blocklist:ads:domains",
        "850-profile:home:1",
        "000-snitchwatch-x",
    ] {
        assert_eq!(
            BulkPlan::new(vec![planned("fine"), named(name)]),
            Err(PlanError::ReservedName(name.into())),
            "{name}"
        );
    }
}

#[test]
fn the_learned_prefixes_are_accepted_and_are_not_the_reserved_one() {
    assert!(!ALLOW_NAME_PREFIX.starts_with("snitchwatch-allow-"));
    assert!(!BLOCK_NAME_PREFIX.starts_with("snitchwatch-allow-"));
    let name = format!("{ALLOW_NAME_PREFIX}0123456789ab");
    assert!(BulkPlan::new(vec![named(&name)]).is_ok());
}

#[test]
fn names_from_rules_for_pass_the_guard() {
    use crate::learned::Program;
    let program = Program {
        raw_name: "curl".into(),
        raw_path: Some("/usr/bin/curl".into()),
        ..Program::default()
    }
    .normalized();
    let payloads = rules_for(&Choice::AllowAny, &program).expect("rules");
    let rules = payloads
        .into_iter()
        .map(|payload| PlannedRule {
            payload,
            label: "curl".into(),
        })
        .collect();
    assert!(BulkPlan::new(rules).is_ok());
}

#[test]
fn an_empty_plan_and_an_unsendable_name_are_refused() {
    assert_eq!(BulkPlan::new(vec![]), Err(PlanError::Empty));
    let long = "x".repeat(300);
    assert_eq!(
        BulkPlan::new(vec![named(&long)]),
        Err(PlanError::InvalidName(long))
    );
    assert_eq!(
        BulkPlan::new(vec![named("a/b")]),
        Err(PlanError::InvalidName("a/b".into()))
    );
}

#[test]
fn the_order_given_is_the_order_kept() {
    let plan = BulkPlan::new(vec![planned("c"), planned("a"), planned("b")]).unwrap();
    let names: Vec<_> = plan.rules().iter().map(|r| r.name()).collect();
    assert_eq!(names, ["c", "a", "b"]);
}
