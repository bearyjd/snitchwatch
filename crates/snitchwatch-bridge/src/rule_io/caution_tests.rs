//! The preview's default ticks and cautions (P2.7 review H2): a replace
//! that loosens or changes a deny, or changes an allow's reach, starts
//! unticked and says why in plain words, with the rule it replaces shown.

use super::*;
use crate::cache::rules::RulesCache;
use serde_json::{json, Value};
use snitchwatch_proto::protocol::{Operator, Rule};

fn leaf(operand: &str, data: &str) -> Operator {
    Operator {
        r#type: "simple".into(),
        operand: operand.into(),
        data: data.into(),
        ..Default::default()
    }
}

fn bound(action: &str) -> Rule {
    Rule {
        name: "010-x".into(),
        enabled: true,
        action: action.into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "list".into(),
            operand: "list".into(),
            list: vec![
                leaf("process.path", "/usr/bin/curl"),
                leaf("dest.host", "example.com"),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn preview_one(cached: Option<&Rule>, file: Value) -> ImportItem {
    let mut cache = RulesCache::default();
    cache.replace_all(cached.into_iter().cloned().collect());
    let doc = parse_document(json!({
        "format": "snitchwatch.rules", "version": 1, "rules": [file],
    }))
    .unwrap();
    preview(&doc, &cache).unwrap().items.remove(0)
}

fn with(rule: &Rule, change: impl FnOnce(&mut Value)) -> Value {
    let mut value = export_rule(rule);
    change(&mut value);
    value
}

fn assert_cautioned(item: &ImportItem, needle: &str) {
    assert_eq!(item.kind, ImportKind::Replace, "{item:?}");
    assert!(!item.ticked, "should start unticked: {item:?}");
    assert!(
        item.cautions.iter().any(|c| c.contains(needle)),
        "no caution containing {needle:?}: {:?}",
        item.cautions
    );
}

#[test]
fn replaces_that_loosen_or_change_a_deny_start_unticked() {
    let deny = bound("deny");
    let other_host = |v: &mut Value| v["operator"]["operands"][1]["data"] = json!("other.example");
    for (file, needle) in [
        (with(&deny, other_host), "what a blocking rule matches"),
        (
            with(&deny, |v| v["duration"] = json!("until restart")),
            "how long a blocking rule lasts",
        ),
        (
            with(&deny, |v| v["enabled"] = json!(false)),
            "turns a blocking rule off",
        ),
        (with(&bound("allow"), |_| {}), "into an allow"),
    ] {
        assert_cautioned(&preview_one(Some(&deny), file), needle);
    }
    let mut off = deny.clone();
    off.enabled = false;
    assert_cautioned(
        &preview_one(Some(&off), export_rule(&deny)),
        "turns a blocking rule on",
    );
}

#[test]
fn replaces_that_widen_an_allow_start_unticked() {
    let allow = bound("allow");
    let other_host = |v: &mut Value| v["operator"]["operands"][1]["data"] = json!("other.example");
    for (file, needle) in [
        (with(&allow, other_host), "what an allow rule matches"),
        (with(&allow, |v| v["nolog"] = json!(true)), "stops logging"),
        (
            with(&allow, |v| v["precedence"] = json!(true)),
            "overrides other rules",
        ),
    ] {
        assert_cautioned(&preview_one(Some(&allow), file), needle);
    }
    let mut off = allow.clone();
    off.enabled = false;
    assert_cautioned(
        &preview_one(Some(&off), export_rule(&allow)),
        "turns on an allow rule",
    );
}

#[test]
fn tightening_or_cosmetic_replaces_start_ticked() {
    let deny = bound("deny");
    for (cached, file) in [
        (bound("allow"), export_rule(&deny)),
        (
            deny.clone(),
            with(&deny, |v| v["description"] = json!("a note")),
        ),
        (deny.clone(), with(&deny, |v| v["action"] = json!("reject"))),
    ] {
        let item = preview_one(Some(&cached), file);
        assert_eq!(item.kind, ImportKind::Replace);
        assert!(item.ticked && item.cautions.is_empty(), "{item:?}");
    }
}

#[test]
fn adds_that_reach_every_app_or_override_others_start_unticked() {
    let host_allow = json!({
        "name": "100-host", "enabled": true, "action": "allow", "duration": "always",
        "operator": { "type": "simple", "operand": "dest.host", "data": "example.com" },
    });
    let item = preview_one(None, host_allow.clone());
    assert!(item.applies_to_all_apps && !item.ticked, "{item:?}");
    assert!(item.cautions.iter().any(|c| c.contains("every app")));

    let precedence = with(&bound("allow"), |v| v["precedence"] = json!(true));
    let item = preview_one(None, precedence);
    assert!(!item.ticked && item.cautions.iter().any(|c| c.contains("overrides")));

    let mut host_deny = host_allow;
    host_deny["action"] = json!("deny");
    let item = preview_one(None, host_deny);
    assert!(
        item.applies_to_all_apps && item.ticked,
        "a deny for every app only blocks more"
    );
}

#[test]
fn a_replace_shows_the_rule_it_replaces() {
    let mut old = bound("deny");
    old.nolog = true;
    let item = preview_one(Some(&old), with(&bound("allow"), |_| {}));
    let previous = item.previous.expect("previous rule");
    assert_eq!(previous.action, "deny");
    assert_eq!(previous.duration, "always");
    assert!(previous.nolog);
    assert_eq!(
        previous.conditions,
        vec!["process.path is /usr/bin/curl", "dest.host is example.com"]
    );
    assert!(preview_one(None, export_rule(&old)).previous.is_none());
}

#[test]
fn changed_fields_and_cautions_are_plain_words() {
    let allow = bound("allow");
    let item = preview_one(
        Some(&allow),
        with(&allow, |v| {
            v["nolog"] = json!(true);
            v["precedence"] = json!(true);
            v["enabled"] = json!(false);
            v["duration"] = json!("until restart");
        }),
    );
    let text = format!("{:?} {:?}", item.changed_fields, item.cautions);
    for raw in ["nolog", "precedence", "enabled", "until restart\""] {
        assert!(!text.contains(raw), "{raw} in {text}");
    }
}

#[test]
fn only_simple_program_conditions_count_as_app_bound() {
    for (operator, all_apps) in [
        (
            json!({ "type": "simple", "operand": "process.parent.path",
                 "data": "/usr/lib/systemd/systemd" }),
            true,
        ),
        (
            json!({ "type": "list", "operands": [
            { "type": "regexp", "operand": "process.path", "data": "^/usr/bin/curl$" },
            { "type": "simple", "operand": "dest.port", "data": "443" } ] }),
            true,
        ),
        (
            json!({ "type": "simple", "operand": "process.command", "data": "curl x" }),
            false,
        ),
    ] {
        let item = preview_one(
            None,
            json!({ "name": "100-x", "enabled": true, "action": "deny",
                    "duration": "always", "operator": operator }),
        );
        assert_eq!(item.applies_to_all_apps, all_apps, "{item:?}");
    }
}

// --- What the preview can't see, and how big the result gets (M1, M3) ----

#[test]
fn a_name_the_cache_left_out_is_refused() {
    let mut cache = RulesCache::default();
    cache.replace_all(Vec::new());
    cache.set_left_out([("010-x".to_string(), 20_000)].into());
    let doc = parse_document(json!({
        "format": "snitchwatch.rules", "version": 1, "rules": [export_rule(&bound("deny"))],
    }))
    .unwrap();
    let item = preview(&doc, &cache).unwrap().items.remove(0);
    assert_eq!(item.kind, ImportKind::Refused);
    assert!(
        item.problems.iter().any(|p| p.reason == HIDDEN_NAME),
        "{item:?}"
    );
}

fn numbered(i: usize, description: &str) -> Rule {
    Rule {
        name: format!("r{i:05}"),
        description: description.into(),
        ..bound("deny")
    }
}

#[test]
fn a_preview_that_would_overflow_the_rule_list_is_refused() {
    let file = |n: usize| {
        parse_document(json!({
            "format": "snitchwatch.rules", "version": 1,
            "rules": (0..n).map(|i| export_rule(&numbered(90_000 + i, ""))).collect::<Vec<_>>(),
        }))
        .unwrap()
    };
    let mut cache = RulesCache::default();
    cache.replace_all(
        (0..crate::cache::rules::MAX_SNAPSHOT_RULES - 3)
            .map(|i| numbered(i, ""))
            .collect(),
    );
    cache.set_left_out([("hidden".to_string(), 100)].into());
    assert!(preview(&file(2), &cache).is_ok(), "exactly at the cap");
    assert_eq!(
        preview(&file(3), &cache).unwrap_err(),
        PreviewError::TooManyRules
    );
}

#[test]
fn a_preview_that_would_overflow_the_daemon_snapshot_is_refused() {
    // About 3.5 MiB of cached rules: twenty more large rules tip it past
    // what the bridge accepts from the daemon (4 MiB, less a margin).
    let big = "d".repeat(16_000);
    let mut cache = RulesCache::default();
    cache.replace_all((0..230).map(|i| numbered(i, &big)).collect());
    let file = |n: usize| {
        parse_document(json!({
            "format": "snitchwatch.rules", "version": 1,
            "rules": (0..n).map(|i| export_rule(&numbered(1_000 + i, &big))).collect::<Vec<_>>(),
        }))
        .unwrap()
    };
    assert!(preview(&file(1), &cache).is_ok());
    assert_eq!(
        preview(&file(20), &cache).unwrap_err(),
        PreviewError::SnapshotTooLarge
    );
    for error in [
        PreviewError::Unavailable,
        PreviewError::TooManyRules,
        PreviewError::SnapshotTooLarge,
    ] {
        assert!(!error.describe().is_empty());
    }
}

// --- Re-review follow-ups ----------------------------------------------------

#[test]
fn an_allow_made_permanent_or_a_deny_unlogged_starts_unticked() {
    let mut restart = bound("allow");
    restart.duration = "until restart".into();
    assert_cautioned(
        &preview_one(Some(&restart), export_rule(&bound("allow"))),
        "permanent",
    );
    let deny = bound("deny");
    assert_cautioned(
        &preview_one(Some(&deny), with(&deny, |v| v["nolog"] = json!(true))),
        "stops logging the connections a blocking rule",
    );
}

/// An allow for an interpreter or launcher, with no destination, lets
/// every script or program it runs reach anywhere.
#[test]
fn an_allow_for_a_launcher_with_no_destination_starts_unticked() {
    let launcher = |path: &str, action: &str| {
        json!({ "name": "100-x", "enabled": true, "action": action, "duration": "always",
                "operator": { "type": "simple", "operand": "process.path", "data": path,
                              "sensitive": true } })
    };
    for path in [
        "/usr/bin/python3",
        "/usr/bin/python3.12",
        "/usr/bin/bash",
        "/bin/sh",
        "/usr/bin/env",
        "/usr/bin/node",
        "/usr/bin/flatpak",
        "/usr/bin/steam",
    ] {
        let item = preview_one(None, launcher(path, "allow"));
        assert!(!item.ticked, "{path}: {item:?}");
        assert!(
            item.cautions
                .iter()
                .any(|c| c.contains("runs other programs")),
            "{path}: {:?}",
            item.cautions
        );
    }
    assert!(preview_one(None, launcher("/usr/bin/curl", "allow")).ticked);
    assert!(preview_one(None, launcher("/usr/bin/python3", "deny")).ticked);
    let mut bounded = launcher("/usr/bin/python3", "allow");
    bounded["operator"] = json!({ "type": "list", "operands": [
        bounded["operator"].clone(),
        { "type": "simple", "operand": "dest.host", "data": "pypi.org" } ] });
    assert!(
        preview_one(None, bounded).ticked,
        "a destination narrows it"
    );
}

// --- Shared with the rule editor (P2.1) -----------------------------------

#[test]
fn edit_cautions_compare_with_the_rule_being_replaced() {
    let deny = bound("deny");
    let mut allow = bound("allow");
    allow.name = "011-renamed".into();
    let cautions = edit_cautions(Some(&deny), &allow);
    assert!(
        cautions.iter().any(|c| c.contains("into an allow")),
        "{cautions:?}"
    );
    assert!(edit_cautions(Some(&deny), &deny).is_empty());
    let mut host_allow = bound("allow");
    host_allow.operator = Some(leaf("dest.host", "example.com"));
    assert!(edit_cautions(None, &host_allow)
        .iter()
        .any(|c| c.contains("every app")));
}

#[test]
fn only_an_enabled_change_is_a_toggle() {
    let cached = bound("deny");
    let mut toggled = cached.clone();
    toggled.enabled = false;
    toggled.created = 0;
    // As a GUI sends it back: a list's operand empty.
    toggled.operator.as_mut().unwrap().operand.clear();
    assert!(only_enabled_differs(&cached, &toggled));
    assert!(only_enabled_differs(&cached, &cached));
    for change in [
        |r: &mut Rule| r.duration = "until restart".into(),
        |r: &mut Rule| r.action = "allow".into(),
        |r: &mut Rule| r.precedence = true,
        |r: &mut Rule| r.nolog = true,
        |r: &mut Rule| r.description = "note".into(),
        |r: &mut Rule| r.operator.as_mut().unwrap().list[1].data = "other".into(),
    ] {
        let mut changed = toggled.clone();
        change(&mut changed);
        assert!(!only_enabled_differs(&cached, &changed), "{changed:?}");
    }
}
