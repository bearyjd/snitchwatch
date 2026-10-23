//! A proposal an existing rule already makes is reported, not offered.

use super::*;
use crate::rules::row_store::Rule;
use serde_json::{json, Value};

const PATH: &str = "/opt/x/app";

fn program() -> Program {
    prog("app", PATH, &[("a.example", 3), ("b.example", 2)])
}

/// The rule the owner (or an earlier batch) already has: the wire form of
/// `choice`'s rule, as the bridge lists it back.
fn existing(choice: &Choice, index: usize) -> Rule {
    let payload = rules_for(choice, &program()).unwrap().remove(index);
    serde_json::from_value(payload.to_wire()).unwrap()
}

fn with(mut rule: Rule, edit: impl FnOnce(&mut Rule)) -> Rule {
    edit(&mut rule);
    rule
}

fn present(rules: &[Rule]) -> ProgramProposal {
    build_review(&[program()], rules, &ReviewConfig::default()).remove(0)
}

fn hosts_choice() -> Choice {
    Choice::AllowHosts(vec!["a.example".into(), "b.example".into()])
}

#[test]
fn an_identical_enabled_rule_is_reported_and_not_offered() {
    let p = present(&[existing(&Choice::AllowAny, 0)]);
    assert_eq!(p.already_present, vec![Choice::AllowAny]);
    assert!(p.choice(ChoiceKind::AllowAny).is_none());
    assert!(p.choice(ChoiceKind::Block).is_some(), "the rest stays");
}

#[test]
fn a_block_rule_that_exists_is_reported() {
    let p = present(&[existing(&Choice::Block, 0)]);
    assert_eq!(p.already_present, vec![Choice::Block]);
    assert!(p.choice(ChoiceKind::Block).is_none());
}

#[test]
fn the_name_does_not_matter_only_the_content() {
    let renamed = with(existing(&Choice::AllowAny, 0), |r| {
        r.name = "my-own-name".into();
        r.description = "typed by hand".into();
    });
    assert_eq!(present(&[renamed]).already_present, vec![Choice::AllowAny]);
}

#[test]
fn a_rule_that_differs_in_any_way_that_matters_is_not_the_same() {
    let base = existing(&Choice::AllowAny, 0);
    type Edit = Box<dyn Fn(&mut Rule)>;
    let edits: Vec<(&str, Edit)> = vec![
        ("disabled", Box::new(|r| r.enabled = false)),
        ("deny", Box::new(|r| r.action = "deny".into())),
        ("five minutes", Box::new(|r| r.duration = "5m".into())),
        (
            "until restart",
            Box::new(|r| r.duration = "until restart".into()),
        ),
        (
            "other path",
            Box::new(|r| r.operator["data"] = json!("/opt/x/other")),
        ),
        (
            "insensitive",
            Box::new(|r| r.operator["sensitive"] = json!(false)),
        ),
        (
            "other operand",
            Box::new(|r| r.operator["operand"] = json!("process.command")),
        ),
        ("regexp", Box::new(|r| r.operator["type"] = json!("regexp"))),
    ];
    for (what, edit) in edits {
        let rule = with(base.clone(), |r| edit(r));
        let p = present(&[rule]);
        assert!(!p.already_present.contains(&Choice::AllowAny), "{what}");
        assert!(p.choice(ChoiceKind::AllowAny).is_some(), "{what}");
    }
}

#[test]
fn an_existing_deny_is_the_block_and_not_the_allow() {
    let deny = with(existing(&Choice::AllowAny, 0), |r| r.action = "deny".into());
    let p = present(&[deny]);
    assert_eq!(p.already_present, vec![Choice::Block]);
    assert!(p.choice(ChoiceKind::AllowAny).is_some());
}

#[test]
fn a_rule_for_another_destination_is_not_the_same() {
    let other_host = existing(&Choice::AllowHosts(vec!["other.example".into()]), 0);
    let p = present(&[other_host]);
    assert!(p.already_present.is_empty());
    assert!(p.choice(ChoiceKind::AllowHosts).is_some());
}

#[test]
fn host_case_and_list_order_do_not_hide_an_identical_rule() {
    let mut rule = existing(&hosts_choice(), 0);
    // The daemon compares hosts without regard to case; the list is an AND.
    rule.operator["operands"][1]["data"] = json!("A.EXAMPLE");
    rule.operator["operands"].as_array_mut().unwrap().reverse();
    let both = existing(&hosts_choice(), 1);
    let p = present(&[rule, both]);
    assert_eq!(p.already_present, vec![hosts_choice()]);
}

#[test]
fn a_set_of_host_rules_is_present_only_when_every_one_is() {
    let only_first = present(&[existing(&hosts_choice(), 0)]);
    assert!(only_first.already_present.is_empty());
    assert!(
        only_first.choice(ChoiceKind::AllowHosts).is_some(),
        "offered whole"
    );
    let both = present(&[existing(&hosts_choice(), 0), existing(&hosts_choice(), 1)]);
    assert_eq!(both.already_present, vec![hosts_choice()]);
    assert!(both.choice(ChoiceKind::AllowHosts).is_none());
    assert!(!both.selected_by_suggest);
}

#[test]
fn a_present_choice_is_never_selected_by_suggest() {
    let system = prog("app", "/usr/bin/app", &[("a.example", 3)]);
    let rule: Rule = serde_json::from_value(
        rules_for(&Choice::AllowHosts(vec!["a.example".into()]), &system).unwrap()[0].to_wire(),
    )
    .unwrap();
    let p = build_review(&[system], &[rule], &ReviewConfig::default()).remove(0);
    assert!(!p.selected_by_suggest);
    assert_eq!(p.already_present.len(), 1);
}

#[test]
fn an_unreadable_operator_in_the_list_is_no_match() {
    let junk = Rule {
        operator: Value::Null,
        ..existing(&Choice::AllowAny, 0)
    };
    assert!(present(&[junk]).already_present.is_empty());
    assert!(present(&[Rule::default()]).already_present.is_empty());
}

#[test]
fn a_directory_rule_that_exists_is_reported() {
    let versioned = prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[("a.example", 1)],
    );
    let choice = Choice::DirectoryRule(directory_rule(&versioned).unwrap());
    let rule: Rule =
        serde_json::from_value(rules_for(&choice, &versioned).unwrap()[0].to_wire()).unwrap();
    let p = build_review(&[versioned], &[rule], &ReviewConfig::default()).remove(0);
    assert_eq!(p.already_present, vec![choice]);
}
