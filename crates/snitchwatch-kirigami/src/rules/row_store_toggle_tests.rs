//! Prompt-slot D: a recommended background-service rule is read-only, can't
//! be deleted, but can be turned on or off (the bridge's `toggleable`).

use super::*;
use serde_json::Value;

fn wire(name: &str, reason: Option<&str>, deletable: bool, toggleable: Option<bool>) -> Value {
    let mut rule = serde_json::json!({
        "name": name, "enabled": true, "action": "allow", "duration": "always",
        "description": "", "precedence": false, "nolog": false,
        "operator": { "type": "simple", "operand": "dest.host", "data": "x.example",
                      "sensitive": false },
        "deletable": deletable,
    });
    if let Some(reason) = reason {
        rule["readOnlyReason"] = reason.into();
    }
    if let Some(toggleable) = toggleable {
        rule["toggleable"] = toggleable.into();
    }
    rule
}

#[test]
fn a_toggleable_read_only_rule_can_be_turned_on_or_off_only() {
    let mut s = RulesStore::new();
    let reason = "A recommended background-service rule.";
    s.apply(&ServerMessage::SetRules {
        rules: vec![
            wire("snitchwatch-default-x", Some(reason), false, Some(true)),
            wire("z00-blocklist:ads:1", Some(reason), false, Some(false)),
            // A bridge that predates the flag: toggleable unless read-only.
            wire("old-locked", Some(reason), false, None),
            wire("old-editable", None, true, None),
        ],
    });
    let off = s
        .rule_json_with_enabled("snitchwatch-default-x", false)
        .expect("a toggle is built");
    assert_eq!(off["enabled"], false);
    assert!(off.get("toggleable").is_none(), "never sent back");
    assert!(!s.is_deletable("snitchwatch-default-x"));
    assert!(s
        .rule_json_with_enabled("z00-blocklist:ads:1", false)
        .is_none());
    assert!(s.rule_json_with_enabled("old-locked", false).is_none());
    assert!(s.rule_json_with_enabled("old-editable", false).is_some());

    let found = |name: &str| -> Value {
        serde_json::from_str(&found_rule_json(&s, name).unwrap()).unwrap()
    };
    assert_eq!(found("snitchwatch-default-x")["toggleable"], true);
    assert_eq!(found("snitchwatch-default-x")["readOnlyReason"], reason);
    assert_eq!(found("z00-blocklist:ads:1")["toggleable"], false);
    assert_eq!(found("old-locked")["toggleable"], false);
    assert_eq!(found("old-editable")["toggleable"], true);
}
