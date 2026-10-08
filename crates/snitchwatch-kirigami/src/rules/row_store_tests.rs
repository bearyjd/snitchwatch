use super::*;

fn rule(name: &str, enabled: bool, action: &str) -> Rule {
    Rule {
        name: name.to_string(),
        enabled,
        action: action.to_string(),
        duration: "always".to_string(),
        description: String::new(),
        operator: serde_json::json!({"operand": "dest.host", "data": "example.com"}),
        precedence: false,
        nolog: false,
        created: 0,
        display_name: None,
        read_only_reason: None,
        deletable: None,
        user_names: Default::default(),
        toggleable: None,
    }
}

fn names(store: &RulesStore) -> Vec<String> {
    store.rules().iter().map(|r| r.name.clone()).collect()
}

#[test]
fn set_rules_replaces_the_list() {
    let mut s = RulesStore::new();
    assert!(s.apply(&ServerMessage::SetRules {
        rules: vec![
            serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
            serde_json::to_value(rule("z00-blocklist:ads:0001-x.example", true, "deny")).unwrap(),
        ],
    }));
    assert_eq!(
        names(&s),
        vec!["899-firefox", "z00-blocklist:ads:0001-x.example"]
    );

    // A second SetRules replaces wholesale.
    assert!(s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::to_value(rule("999-other", true, "deny")).unwrap()],
    }));
    assert_eq!(names(&s), vec!["999-other"]);
}

#[test]
fn update_rules_upserts_by_name() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
    });

    // Update existing.
    assert!(s.apply(&ServerMessage::UpdateRules {
        rules: vec![serde_json::to_value(rule("899-firefox", false, "allow")).unwrap()],
    }));
    assert!(!s.find_by_name("899-firefox").unwrap().enabled);

    // Insert new.
    assert!(s.apply(&ServerMessage::UpdateRules {
        rules: vec![serde_json::to_value(rule("999-new", true, "deny")).unwrap()],
    }));
    assert_eq!(names(&s), vec!["899-firefox", "999-new"]);
}

/// The bridge re-sends the whole list after every rule command, so the
/// same list again is not a change: the Rules page keeps what it computed
/// from the list (an analysis) only while the list is the same.
#[test]
fn the_same_list_again_is_not_a_change() {
    let mut s = RulesStore::new();
    let list = || {
        vec![
            serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
            serde_json::to_value(rule("999-other", true, "deny")).unwrap(),
        ]
    };
    assert!(s.apply(&ServerMessage::SetRules { rules: list() }));
    assert!(!s.apply(&ServerMessage::SetRules { rules: list() }));
    assert!(!s.apply(&ServerMessage::UpdateRules {
        rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
    }));
    // A change anywhere is one: a field, an addition, a removal.
    assert!(s.apply(&ServerMessage::UpdateRules {
        rules: vec![serde_json::to_value(rule("899-firefox", false, "allow")).unwrap()],
    }));
    let mut again = list();
    again.pop();
    assert!(s.apply(&ServerMessage::SetRules { rules: again }));
    assert_eq!(names(&s), vec!["899-firefox"]);
    assert!(s.apply(&ServerMessage::SetRules {
        rules: vec![
            serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
            serde_json::to_value(rule("999-other", true, "deny")).unwrap(),
        ]
    }));
}

/// Issue #48: the bridge re-sends the daemon's full list after every
/// confirmed or refused command, so a later `SetRules` must replace
/// rules an earlier `UpdateRules` added (e.g. one the daemon refused).
#[test]
fn set_rules_after_update_rules_replaces_the_list() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
    });
    s.apply(&ServerMessage::UpdateRules {
        rules: vec![serde_json::to_value(rule("999-new", true, "deny")).unwrap()],
    });

    assert!(s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::to_value(rule("899-firefox", false, "allow")).unwrap()],
    }));

    assert_eq!(names(&s), vec!["899-firefox"]);
    assert!(!s.find_by_name("899-firefox").unwrap().enabled);
}

#[test]
fn unrelated_message_does_not_change_rules() {
    let mut s = RulesStore::new();
    assert!(!s.apply(&ServerMessage::ClearConnectionRows));
}

#[test]
fn malformed_rule_values_are_dropped_not_fatal() {
    let mut s = RulesStore::new();
    assert!(s.apply(&ServerMessage::SetRules {
        rules: vec![
            serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
            serde_json::Value::Null,
            serde_json::json!("not an object"),
        ],
    }));
    // Null/string entries are not a struct shape at all, so they fail to
    // deserialize and are dropped by `filter_map` — only the well-formed
    // object survives. Malformed entries never panic the store.
    assert_eq!(names(&s), vec!["899-firefox"]);
}

#[test]
fn user_rule_source_is_user() {
    let r = rule("899-firefox-allow-out", true, "allow");
    assert_eq!(r.source(), RuleSource::User);
    assert!(!r.is_blocklist_sourced());
}

#[test]
fn blocklist_band_name_detected_by_prefix() {
    let r = rule(
        "z00-blocklist:stevenblack:0001-doubleclick.net",
        true,
        "deny",
    );
    assert_eq!(
        r.source(),
        RuleSource::Blocklist {
            list_id: "stevenblack".to_string()
        }
    );
    assert!(r.is_blocklist_sourced());
}

#[test]
fn legacy_blocklist_band_name_still_detected() {
    // Migration-window tolerance: a stale pre-migration daemon may still
    // surface a "900-blocklist:" rule before the bridge purges it. It must
    // still group as blocklist-sourced, not masquerade as a user rule.
    let r = rule(
        "900-blocklist:stevenblack:0001-doubleclick.net",
        true,
        "deny",
    );
    assert_eq!(
        r.source(),
        RuleSource::Blocklist {
            list_id: "stevenblack".to_string()
        }
    );
    assert!(r.is_blocklist_sourced());
}

/// Compared exactly, as the daemon compares (PR #106 review N4): only
/// `allow`, `deny` and `reject` are actions it recognises.
#[test]
fn normalized_action_compares_exactly() {
    assert_eq!(rule("r", true, "allow").normalized_action(), "allow");
    assert_eq!(rule("r", true, "deny").normalized_action(), "deny");
    assert_eq!(rule("r", true, "reject").normalized_action(), "deny");
    for unknown in ["ALLOW", "Deny", "drop", ""] {
        assert_eq!(
            rule("r", true, unknown).normalized_action(),
            UNRECOGNISED_ACTION,
            "{unknown:?}"
        );
    }
}

#[test]
fn operator_summary_simple_shape() {
    let r = rule("r", true, "deny");
    assert_eq!(r.operator_summary(), "dest.host = example.com");
}

#[test]
fn operator_summary_tagged_wrapper_shape() {
    let mut r = rule("r", true, "deny");
    r.operator = serde_json::json!({
        "simple": {"operand": "process.path", "data": "/usr/bin/firefox"}
    });
    assert_eq!(r.operator_summary(), "process.path = /usr/bin/firefox");
}

#[test]
fn operator_summary_list_shape_joins_children() {
    let mut r = rule("r", true, "deny");
    r.operator = serde_json::json!({
        "operands": [
            {"operand": "process.path", "data": "/usr/bin/firefox"},
            {"simple": {"operand": "dest.port", "data": "443"}}
        ]
    });
    assert_eq!(
        r.operator_summary(),
        "process.path = /usr/bin/firefox AND dest.port = 443"
    );
}

#[test]
fn operator_summary_unrecognized_shape_is_empty() {
    let mut r = rule("r", true, "deny");
    r.operator = serde_json::json!("garbage");
    assert_eq!(r.operator_summary(), "");
    r.operator = serde_json::Value::Null;
    assert_eq!(r.operator_summary(), "");
}

#[test]
fn rule_json_with_enabled_sets_enabled_and_preserves_other_fields() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
    });
    let toggled = s
        .rule_json_with_enabled("899-firefox", false)
        .expect("rule known");
    assert_eq!(toggled["enabled"], false);
    assert_eq!(toggled["name"], "899-firefox");
    assert_eq!(toggled["action"], "allow");
}

/// #48: two clicks before the bridge's next `SetRules` must send "off"
/// then "on", not "off" twice (a flip of the stale stored value).
#[test]
fn a_quick_second_click_sends_the_value_the_switch_shows() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
    });
    let first = s.rule_json_with_enabled("899-firefox", false).unwrap();
    let second = s.rule_json_with_enabled("899-firefox", true).unwrap();
    assert_eq!(first["enabled"], false);
    assert_eq!(second["enabled"], true);
}

#[test]
fn display_name_is_shown_but_never_sent_back() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![
            serde_json::json!({
                "name": "evil\u{202e}txt.exe", "displayName": "eviltxt.exe",
                "enabled": true, "action": "allow", "duration": "always",
                "description": "", "operator": {}, "precedence": false, "nolog": false,
            }),
            serde_json::to_value(rule("old-bridge", true, "allow")).unwrap(),
        ],
    });
    let name = s.rules()[0].name.clone();
    assert_eq!(s.rules()[0].shown_name(), "eviltxt.exe");
    assert_eq!(
        s.rules()[1].shown_name(),
        "old-bridge",
        "no displayName: name"
    );
    let sent = s.rule_json_with_enabled(&name, false).unwrap();
    assert_eq!(sent["name"], name.as_str(), "commands keep the exact name");
    assert!(sent.get("displayName").is_none());
    let found: serde_json::Value =
        serde_json::from_str(&found_rule_json(&s, &name).unwrap()).unwrap();
    assert_eq!(found["displayName"], "eviltxt.exe");
}

/// A toggle is sent to the daemon as a `CHANGE_RULE` carrying the whole
/// rule, and the daemon's handler does a wholesale `Replace`. So any field
/// this store drops on ingest is a field the next toggle silently clears on
/// the daemon — and for `precedence` that quietly changes which rule wins
/// for traffic the user wasn't touching.
///
/// This is a real regression: `precedence`/`nolog` were absent from this
/// struct when the CHANGE_RULE path was first wired, so every toggle reset
/// them to false.
#[test]
fn toggling_preserves_precedence_and_nolog_through_the_wire() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![serde_json::json!({
            "name": "010-priority-allow",
            "enabled": true,
            "action": "allow",
            "duration": "always",
            "description": "",
            "operator": {"operand": "dest.host", "data": "example.com"},
            "precedence": true,
            "nolog": true,
        })],
    });

    // Ingest must not drop them...
    let ingested = s.find_by_name("010-priority-allow").expect("rule known");
    assert!(ingested.precedence, "precedence lost on SetRules ingest");
    assert!(ingested.nolog, "nolog lost on SetRules ingest");

    // ...and the JSON sent back to the daemon must still carry them.
    let toggled = s
        .rule_json_with_enabled("010-priority-allow", false)
        .expect("rule known");
    assert_eq!(toggled["enabled"], false, "the toggle itself must apply");
    assert_eq!(
        toggled["precedence"], true,
        "toggling a rule must not clear its precedence on the daemon"
    );
    assert_eq!(
        toggled["nolog"], true,
        "toggling a rule must not clear its nolog flag on the daemon"
    );
}

/// Option (b) of the #48 review: a rule Snitchwatch can't edit stays
/// listed with its reason, and no command is ever built for it.
#[test]
fn a_read_only_rule_is_listed_with_its_reason_but_never_commanded() {
    let mut s = RulesStore::new();
    let mut locked = serde_json::to_value(rule("stock\\ui", true, "deny")).unwrap();
    locked["readOnlyReason"] = "Snitchwatch can't edit this rule.".into();
    s.apply(&ServerMessage::SetRules {
        rules: vec![
            locked,
            serde_json::to_value(rule("899-curl", true, "allow")).unwrap(),
        ],
    });

    assert_eq!(names(&s), vec!["stock\\ui", "899-curl"]);
    assert!(s.rules()[0].is_read_only());
    assert!(s.rule_json_with_enabled("stock\\ui", false).is_none());
    assert!(!s.is_deletable("stock\\ui"));
    assert!(s.is_deletable("899-curl"));
    assert!(!s.is_deletable("unknown"));
    let found: serde_json::Value =
        serde_json::from_str(&found_rule_json(&s, "stock\\ui").unwrap()).unwrap();
    assert_eq!(found["readOnlyReason"], "Snitchwatch can't edit this rule.");
    let editable: serde_json::Value =
        serde_json::from_str(&found_rule_json(&s, "899-curl").unwrap()).unwrap();
    assert_eq!(editable["readOnlyReason"], "");
    let sent = serde_json::to_value(&s.rules()[0]).unwrap();
    assert!(sent.get("readOnlyReason").is_none(), "never sent back");
}

/// A rule read-only only for its conditions can still be deleted: a
/// delete names the rule and nothing else. A bad name can't be.
#[test]
fn delete_follows_the_bridges_deletable_flag() {
    let mut s = RulesStore::new();
    let mut shape = serde_json::to_value(rule("899-lan", true, "allow")).unwrap();
    shape["readOnlyReason"] = "Snitchwatch can't change this rule.".into();
    shape["deletable"] = true.into();
    let mut name = serde_json::to_value(rule("stock\\ui", true, "deny")).unwrap();
    name["readOnlyReason"] = "Snitchwatch can't change or delete this rule.".into();
    name["deletable"] = false.into();
    let mut legacy_locked = serde_json::to_value(rule("old-locked", true, "deny")).unwrap();
    legacy_locked["readOnlyReason"] = "Snitchwatch can't edit this rule.".into();
    let legacy = serde_json::to_value(rule("old-editable", true, "deny")).unwrap();
    s.apply(&ServerMessage::SetRules {
        rules: vec![shape, name, legacy_locked, legacy],
    });

    assert!(s.is_deletable("899-lan"));
    assert!(
        s.rule_json_with_enabled("899-lan", false).is_none(),
        "still read-only"
    );
    assert!(!s.is_deletable("stock\\ui"));
    // A bridge that predates the flag: deletable unless read-only.
    assert!(!s.is_deletable("old-locked"));
    assert!(s.is_deletable("old-editable"));

    let found = |name: &str| -> serde_json::Value {
        serde_json::from_str(&found_rule_json(&s, name).unwrap()).unwrap()
    };
    assert_eq!(found("899-lan")["deletable"], true);
    assert_eq!(found("stock\\ui")["deletable"], false);
    assert_eq!(found("old-editable")["deletable"], true);
    let sent = serde_json::to_value(&s.rules()[0]).unwrap();
    assert!(sent.get("deletable").is_none(), "never sent back");
}

#[test]
fn rule_json_with_enabled_unknown_name_is_none() {
    let s = RulesStore::new();
    assert!(s.rule_json_with_enabled("nope", false).is_none());
}

#[test]
fn found_rule_json_returns_the_rule_shape_for_a_known_name() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![
            serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
            serde_json::to_value(rule("z00-blocklist:ads:0001-x.example", true, "deny")).unwrap(),
        ],
    });
    let json = found_rule_json(&s, "z00-blocklist:ads:0001-x.example").expect("known rule");
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["name"], "z00-blocklist:ads:0001-x.example");
    assert_eq!(parsed["precedence"], 1);
    assert_eq!(parsed["source"], "blocklist");
    assert_eq!(parsed["blocklistId"], "ads");
    assert_eq!(parsed["action"], "deny");
}

#[test]
fn found_rule_json_is_none_for_an_unknown_name() {
    let s = RulesStore::new();
    assert!(found_rule_json(&s, "nope").is_none());
}

#[test]
fn index_of_finds_the_rules_precedence_position() {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules {
        rules: vec![
            serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
            serde_json::to_value(rule("z00-blocklist:ads:0001-x.example", true, "deny")).unwrap(),
        ],
    });
    assert_eq!(s.index_of("899-firefox"), Some(0));
    assert_eq!(s.index_of("z00-blocklist:ads:0001-x.example"), Some(1));
    assert_eq!(s.index_of("nope"), None);
}
