//! Tests for [`super::editor_view`]: what the editor sends, when it may,
//! and what each result says.

use super::editor::{check, new_draft, Condition, MatchKind, RuleDraft};
use super::editor_view::*;
use serde_json::json;
use snitchwatch_bridge::rule_policy::RuleProblem;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome, ServerMessage};

fn curl_draft() -> RuleDraft {
    RuleDraft {
        name: "899-curl".into(),
        conditions: vec![Condition {
            operand: "process.path".into(),
            kind: MatchKind::Exact,
            value: "/usr/bin/curl".into(),
            case_sensitive: true,
        }],
        ..new_draft()
    }
}

#[test]
fn a_new_rule_is_an_add_and_an_edit_names_the_rule_it_changes() {
    let draft = curl_draft();
    match submit_message(&draft, "", "7-1".into()) {
        ClientMessage::AddRule {
            rule, request_id, ..
        } => {
            assert_eq!(rule, draft.to_wire());
            assert_eq!(request_id.as_deref(), Some("7-1"));
        }
        other => panic!("{other:?}"),
    }
    // Renamed in the sheet: the update still names the rule being edited.
    match submit_message(&draft, "899-old", "7-2".into()) {
        ClientMessage::UpdateRule {
            rule_id,
            rule,
            request_id,
            ..
        } => {
            assert_eq!(rule_id, "899-old");
            assert_eq!(rule["name"], "899-curl");
            assert_eq!(request_id.as_deref(), Some("7-2"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn problems_block_saving_and_cautions_need_a_second_click() {
    let fine = check(&curl_draft(), None);
    assert_eq!(may_submit(&fine, false), Ok(()));

    let mut broken = curl_draft();
    broken.duration = "1.5h".into();
    let broken = check(&broken, None);
    assert_eq!(may_submit(&broken, true), Err(FIX_FIRST));

    let old = curl_draft().to_wire();
    let loosened = RuleDraft {
        action: "allow".into(),
        ..curl_draft()
    };
    let loosened = check(&loosened, Some(&old));
    assert!(!loosened.cautions.is_empty());
    assert_eq!(may_submit(&loosened, false), Err(CONFIRM_CAUTIONS));
    assert_eq!(may_submit(&loosened, true), Ok(()));
}

#[test]
fn each_result_says_what_happened_in_plain_words() {
    let saved = finished(&RuleCommandOutcome::Ok);
    assert!(saved.saved);

    let unknown = finished(&RuleCommandOutcome::Timeout);
    assert!(!unknown.saved);
    assert!(
        unknown.status.contains("may have been saved"),
        "{}",
        unknown.status
    );

    let rejected = finished(&RuleCommandOutcome::Rejected {
        reason: "<b>bad regexp</b>".into(),
    });
    assert!(!rejected.saved);
    assert!(rejected.status.contains("<b>bad regexp</b>"));

    let refused = finished(&RuleCommandOutcome::Refused {
        problems: vec![RuleProblem {
            path: "operator.list[1].data".into(),
            reason: "not a port".into(),
        }],
    });
    assert!(!refused.saved);
    assert!(
        refused.status.contains("not a port (condition 2's value)"),
        "{}",
        refused.status
    );
    assert!(!refused.status.contains("operator"), "{}", refused.status);

    let offline = finished(&RuleCommandOutcome::NoDaemon);
    assert!(!offline.saved && offline.status.contains("nothing was sent"));

    let unsure = finished(&RuleCommandOutcome::Unsure {
        reason: "both may exist".into(),
    });
    assert!(!unsure.saved && unsure.status.contains("both may exist"));
}

#[test]
fn only_the_awaited_result_counts() {
    let result = |id: &str| ServerMessage::RuleCommandResult {
        request_id: id.into(),
        outcome: RuleCommandOutcome::Ok,
    };
    assert!(awaits(Some("7-1"), &result("7-1")));
    assert!(!awaits(Some("7-1"), &result("7-2")));
    assert!(!awaits(None, &result("7-1")));
    assert!(interests_rule_editor(&result("x")));
    assert!(!interests_rule_editor(&ServerMessage::SetRules {
        rules: vec![]
    }));
}

#[test]
fn edit_is_offered_only_for_rules_the_editor_can_change() {
    let ok = editable(curl_draft().to_wire(), None);
    assert_eq!(ok.not_editable, "");
    assert_eq!(ok.rule, curl_draft().to_wire());

    let read_only = editable(curl_draft().to_wire(), Some("This is a blocklist rule."));
    assert!(
        read_only.not_editable.starts_with(NOT_EDITABLE),
        "{}",
        read_only.not_editable
    );
    assert!(read_only.not_editable.contains("blocklist"));

    let hashed = json!({
        "name": "899-x", "enabled": true, "action": "deny", "duration": "always",
        "operator": { "type": "list", "operand": "list", "operands": [
            { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl" },
            { "type": "simple", "operand": "process.hash.md5",
              "data": "d41d8cd98f00b204e9800998ecf8427e" },
        ] },
    });
    let hashed = editable(hashed, None);
    assert!(
        hashed.not_editable.starts_with(NOT_EDITABLE),
        "{}",
        hashed.not_editable
    );
}

#[test]
fn a_cached_rule_is_edited_in_its_wire_form() {
    let mut store = super::row_store::RulesStore::new();
    let mut locked = curl_draft().to_wire();
    locked["name"] = json!("z00-blocklist:ads:domains");
    locked["readOnlyReason"] = json!("This rule belongs to a blocklist.");
    let mut shown = curl_draft().to_wire();
    shown["displayName"] = json!("899-curl");
    store.apply(&ServerMessage::SetRules {
        rules: vec![shown, locked],
    });

    let found = editable_in(&store, "899-curl").expect("cached");
    assert_eq!(found.not_editable, "");
    assert_eq!(RuleDraft::from_wire(&found.rule).unwrap(), curl_draft());
    assert!(found.rule.get("displayName").is_none(), "{}", found.rule);

    let locked = editable_in(&store, "z00-blocklist:ads:domains").expect("cached");
    assert!(
        locked.not_editable.contains("belongs to a blocklist"),
        "{}",
        locked.not_editable
    );
    assert!(editable_in(&store, "899-missing").is_none());
}
