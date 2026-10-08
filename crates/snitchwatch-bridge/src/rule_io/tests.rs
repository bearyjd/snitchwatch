//! Tests for [`super`]: the export filter, the envelope, and the preview's
//! classification and flags.

use super::*;
use crate::cache::rules::{RulesCache, MAX_SNAPSHOT_RULES};
use serde_json::{json, Value};
use snitchwatch_proto::protocol::{Operator, Rule};

fn leaf(r#type: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: r#type.into(),
        operand: operand.into(),
        data: data.into(),
        ..Default::default()
    }
}

fn daemon_rule(name: &str, action: &str, operator: Operator) -> Rule {
    Rule {
        created: 1_800_000_000,
        name: name.into(),
        enabled: true,
        action: action.into(),
        duration: "always".into(),
        operator: Some(operator),
        ..Default::default()
    }
}

fn app_bound(name: &str, action: &str) -> Rule {
    // As the daemon reports it: `Compile` sets a list's operand to "list".
    daemon_rule(
        name,
        action,
        Operator {
            r#type: "list".into(),
            operand: "list".into(),
            list: vec![
                Operator {
                    sensitive: true,
                    ..leaf("simple", "process.path", "/usr/bin/curl")
                },
                leaf("simple", "dest.host", "example.com"),
            ],
            ..Default::default()
        },
    )
}

fn synced(rules: Vec<Rule>) -> RulesCache {
    let mut cache = RulesCache::default();
    cache.replace_all(rules);
    cache
}

fn document(rules: Vec<Value>) -> Value {
    json!({ "format": "snitchwatch.rules", "version": 1, "exportedAtUnixMs": 1, "rules": rules })
}

fn wire(rule: &Rule) -> Value {
    export_rule(rule)
}

fn host_rule(name: &str, action: &str) -> Value {
    json!({
        "name": name, "enabled": true, "action": action, "duration": "always",
        "operator": { "type": "simple", "operand": "dest.host", "data": "example.org" },
    })
}

fn previewed(doc: Value, cache: &RulesCache) -> Vec<ImportItem> {
    let doc = parse_document(doc).expect("valid document");
    preview(&doc, cache).expect("cache synced").items
}

fn item<'a>(items: &'a [ImportItem], name: &str) -> &'a ImportItem {
    items.iter().find(|i| i.name == name).expect(name)
}

// --- Export ---------------------------------------------------------------

#[test]
fn export_is_unavailable_until_rules_load() {
    assert_eq!(export(&RulesCache::default(), 1), Err(ExportUnavailable));
}

#[test]
fn export_round_trips_without_display_fields() {
    let mut precedence = app_bound("010-curl-allow", "allow");
    precedence.precedence = true;
    precedence.nolog = true;
    precedence.description = "kept".into();
    let restart = Rule {
        duration: "until restart".into(),
        ..daemon_rule("020-host", "deny", leaf("simple", "dest.host", "x.example"))
    };
    let cache = synced(vec![restart.clone(), precedence.clone()]);

    let exported = export(&cache, 42).unwrap();
    assert_eq!(exported.omitted, OmittedCounts::default());
    let text = serde_json::to_string(&exported.document).unwrap();
    for display in [
        "displayName",
        "readOnlyReason",
        "deletable",
        "toggleable",
        "created",
    ] {
        assert!(!text.contains(display), "{display} leaked into {text}");
    }
    let parsed = parse_document_text(&text).unwrap();
    assert_eq!(parsed, exported.document);
    assert_eq!(parsed.exported_at_unix_ms, 42);
    let names: Vec<_> = parsed.rules.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(names, vec![json!("010-curl-allow"), json!("020-host")]);

    // Imported back into the same rules, everything is unchanged.
    let items = preview(&parsed, &cache).unwrap().items;
    assert!(
        items.iter().all(|i| i.kind == ImportKind::Unchanged),
        "{items:?}"
    );
}

#[test]
fn export_leaves_out_and_counts_what_import_would_refuse() {
    let timed = |name: &str, duration: &str| Rule {
        duration: duration.into(),
        ..daemon_rule(name, "allow", leaf("simple", "dest.host", "a.example"))
    };
    let cache = synced(vec![
        daemon_rule("keep", "deny", leaf("simple", "dest.host", "a.example")),
        timed("once", "once"),
        timed("five", "5m"),
        daemon_rule(
            "z00-blocklist:ads:domains",
            "deny",
            leaf("lists", "lists.domains", "/var/lib/x"),
        ),
        daemon_rule(
            "900-blocklist:ads:domains",
            "deny",
            leaf("simple", "true", ""),
        ),
        daemon_rule(
            "snitchwatch-default-steam",
            "allow",
            leaf("simple", "dest.host", "s"),
        ),
        daemon_rule(
            "000-snitchwatch-fetch",
            "allow",
            leaf("simple", "dest.host", "f"),
        ),
        daemon_rule(
            "stock\\ui",
            "deny",
            leaf("simple", "dest.host", "a.example"),
        ),
        daemon_rule("lan", "allow", leaf("network", "dest.network", "LAN")),
        daemon_rule("all", "deny", leaf("simple", "true", "")),
    ]);
    let exported = export(&cache, 1).unwrap();
    let names: Vec<_> = exported
        .document
        .rules
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("keep")]);
    assert_eq!(
        exported.omitted,
        OmittedCounts {
            once: 1,
            timed: 1,
            managed: 4,
            unsupported_name: 1,
            unsupported_rule: 2,
        }
    );
}

// --- Envelope -------------------------------------------------------------

#[test]
fn envelope_refusals_have_fixed_reasons() {
    let mut v2 = document(Vec::new());
    v2["version"] = json!(2);
    v2["futureKey"] = json!(true);
    assert_eq!(parse_document(v2), Err(DocumentError::Newer));

    let mut v0 = document(Vec::new());
    v0["version"] = json!(0);
    assert_eq!(parse_document(v0), Err(DocumentError::UnsupportedVersion));

    let mut unknown = document(Vec::new());
    unknown["blocklists"] = json!([]);
    assert_eq!(parse_document(unknown), Err(DocumentError::Malformed));

    let mut other = document(Vec::new());
    other["format"] = json!("opensnitch.rules");
    assert_eq!(parse_document(other), Err(DocumentError::NotRulesFile));
    assert_eq!(
        parse_document(json!([1, 2])),
        Err(DocumentError::NotRulesFile)
    );

    let too_many = vec![json!({}); MAX_SNAPSHOT_RULES + 1];
    assert_eq!(
        parse_document(document(too_many)),
        Err(DocumentError::TooManyRules)
    );
    assert!(parse_document(document(vec![json!({}); MAX_SNAPSHOT_RULES])).is_ok());

    for error in [
        DocumentError::TooLarge,
        DocumentError::NotJson,
        DocumentError::Newer,
        DocumentError::Malformed,
    ] {
        assert!(!error.describe().is_empty());
    }
    assert_eq!(
        DocumentError::Newer.describe(),
        "This file was made by a newer Snitchwatch."
    );
}

#[test]
fn a_document_over_the_size_cap_is_refused_before_parsing() {
    let pad = "x".repeat(MAX_DOCUMENT_BYTES);
    let text = format!(r#"{{"format":"snitchwatch.rules","version":1,"rules":[],"pad":"{pad}"}}"#);
    assert!(text.len() > MAX_DOCUMENT_BYTES);
    assert_eq!(parse_document_text(&text), Err(DocumentError::TooLarge));
    assert_eq!(parse_document_text("{"), Err(DocumentError::NotJson));
    assert_eq!(
        MAX_DOCUMENT_BYTES,
        960 * 1024,
        "TooLarge's text names the limit"
    );
    assert!(DocumentError::TooLarge.describe().contains("960 KiB"));
    // A document at the cap must fit one client WebSocket message.
    const {
        assert!(
            MAX_DOCUMENT_BYTES + ENVELOPE_SLACK_BYTES <= crate::ws_server::MAX_CLIENT_MESSAGE_BYTES
        )
    };
}

/// JSON Schema reads `1.0` as the integer 1; so does the parser.
#[test]
fn a_version_written_as_a_float_is_version_1() {
    let mut doc = document(Vec::new());
    doc["version"] = json!(1.0);
    assert!(parse_document(doc).is_ok());
    let mut newer = document(Vec::new());
    newer["version"] = json!(2.0);
    assert_eq!(parse_document(newer), Err(DocumentError::Newer));
    let mut fraction = document(Vec::new());
    fraction["version"] = json!(1.5);
    assert_eq!(
        parse_document(fraction),
        Err(DocumentError::UnsupportedVersion)
    );
}

/// A rule must say whether it is on: the GUI's rule shape defaults a
/// missing `enabled` to on, `rule_from_wire` to off (review #16).
#[test]
fn a_rule_without_enabled_is_refused() {
    let mut rule = host_rule("100-x", "deny");
    rule.as_object_mut().unwrap().remove("enabled");
    let items = previewed(document(vec![rule]), &synced(Vec::new()));
    assert_eq!(items[0].kind, ImportKind::Refused);
    assert!(
        items[0].problems.iter().any(|p| p.path == "enabled"),
        "{items:?}"
    );
}

#[test]
fn envelope_errors_never_echo_the_file() {
    let mut doc = document(Vec::new());
    doc["MARKER"] = json!("MARKER");
    let error = parse_document(doc).unwrap_err();
    assert!(!error.describe().to_lowercase().contains("marker"));
}

// --- Preview --------------------------------------------------------------

#[test]
fn preview_is_refused_while_rules_are_unknown() {
    let doc = parse_document(document(Vec::new())).unwrap();
    assert_eq!(
        preview(&doc, &RulesCache::default()).unwrap_err(),
        PreviewError::Unavailable
    );
}

#[test]
fn a_file_list_rule_matches_its_daemon_twin() {
    // The file's list has no operand (as `operator_from_wire` reads it); the
    // daemon's says "list" and may carry the stock UI's JSON in `data`.
    let mut cached = app_bound("010-curl", "allow");
    cached.operator.as_mut().unwrap().data = "[{\"stock\":\"ui\"}]".into();
    let file = json!({
        "name": "010-curl", "enabled": true, "action": "allow", "duration": "always",
        "operator": { "type": "list", "operands": [
            { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl", "sensitive": true },
            { "type": "simple", "operand": "dest.host", "data": "example.com" },
        ]},
    });
    let items = previewed(document(vec![file]), &synced(vec![cached]));
    assert_eq!(item(&items, "010-curl").kind, ImportKind::Unchanged);
}

#[test]
fn preview_classifies_add_replace_unchanged_and_refused() {
    let cache = synced(vec![
        app_bound("010-same", "allow"),
        app_bound("020-changed", "allow"),
    ]);
    let mut changed = wire(&app_bound("020-changed", "allow"));
    changed["description"] = json!("now with a note");
    changed["nolog"] = json!(true);
    let items = previewed(
        document(vec![
            wire(&app_bound("010-same", "allow")),
            changed,
            host_rule("030-new", "deny"),
            host_rule("z00-blocklist:x:domains", "allow"),
        ]),
        &cache,
    );
    assert_eq!(item(&items, "010-same").kind, ImportKind::Unchanged);
    let replace = item(&items, "020-changed");
    assert_eq!(replace.kind, ImportKind::Replace);
    assert_eq!(replace.changed_fields, vec!["description", "logging"]);
    assert_eq!(item(&items, "030-new").kind, ImportKind::Add);
    let refused = item(&items, "z00-blocklist:x:domains");
    assert_eq!(refused.kind, ImportKind::Refused);
    assert!(!refused.problems.is_empty());
    assert_eq!(items.len(), 4, "one item per rule, in file order");
    assert_eq!(items[0].index, 0);
}

#[test]
fn duplicate_names_refuse_both_copies() {
    let items = previewed(
        document(vec![host_rule("dup", "deny"), host_rule("dup", "allow")]),
        &synced(Vec::new()),
    );
    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(item.kind, ImportKind::Refused);
        assert!(
            item.problems.iter().any(|p| p.reason == DUPLICATE_NAME),
            "{item:?}"
        );
    }
}

#[test]
fn a_refused_rule_with_an_unusable_name_does_not_show_it() {
    let items = previewed(
        document(vec![
            host_rule("../<b>evil</b>", "deny"),
            json!("not a rule"),
        ]),
        &synced(Vec::new()),
    );
    for item in &items {
        assert_eq!(item.kind, ImportKind::Refused);
        assert_eq!(item.name, "");
        assert!(!item.display_name.contains("evil"), "{item:?}");
    }
}

#[test]
fn weakens_flags_loosening_replaces_only() {
    let deny = app_bound("010-x", "deny");
    let allow = app_bound("010-x", "allow");
    let replace_with = |cached: &Rule, file: Value| {
        let items = previewed(document(vec![file]), &synced(vec![cached.clone()]));
        items[0].clone()
    };

    let to_allow = replace_with(&deny, wire(&allow));
    assert!(to_allow.weakens, "deny -> allow");

    let mut disabled = wire(&deny);
    disabled["enabled"] = json!(false);
    assert!(replace_with(&deny, disabled).weakens, "disabling a deny");

    let mut reject = deny.clone();
    reject.action = "reject".into();
    assert!(
        replace_with(&reject, wire(&allow)).weakens,
        "reject -> allow"
    );

    let mut precedence = wire(&allow);
    precedence["precedence"] = json!(true);
    assert!(
        replace_with(&allow, precedence).weakens,
        "precedence added to an allow"
    );

    assert!(!replace_with(&allow, wire(&deny)).weakens, "allow -> deny");
    let added = previewed(document(vec![wire(&allow)]), &synced(Vec::new()));
    assert!(!added[0].weakens, "an add replaces nothing");
}

#[test]
fn applies_to_all_apps_means_no_process_condition_at_any_depth() {
    let items = previewed(
        document(vec![
            host_rule("host-only", "allow"),
            wire(&app_bound("bound", "allow")),
        ]),
        &synced(Vec::new()),
    );
    assert!(item(&items, "host-only").applies_to_all_apps);
    assert!(!item(&items, "bound").applies_to_all_apps);
}

#[test]
fn preview_shows_what_will_be_installed() {
    let mut rule = host_rule("restart-only", "deny");
    rule["duration"] = json!("until restart");
    rule["precedence"] = json!(true);
    rule["operator"] = json!({
        "type": "regexp", "operand": "dest.host", "data": "^ex\u{202e}ample\\.com$",
        "sensitive": true,
    });
    let items = previewed(document(vec![rule]), &synced(Vec::new()));
    let shown = &items[0];
    assert!(!shown.persists, "until restart is lost at a daemon restart");
    assert!(shown.precedence);
    assert_eq!(shown.action, "deny");
    assert_eq!(shown.duration, "until restart");
    assert_eq!(shown.conditions.len(), 1);
    let condition = &shown.conditions[0];
    assert!(condition.starts_with("dest.host matches"), "{condition}");
    assert!(condition.contains("case-sensitive"), "{condition}");
    assert!(!condition.contains('\u{202e}'), "{condition}");
    assert!(condition.contains("hidden characters"), "{condition}");
}

/// A long condition is shown whole: a narrow-looking start could otherwise
/// hide the part that broadens it.
#[test]
fn a_long_condition_is_shown_in_full() {
    let pattern = format!("^(?:{})\\.example$|^(?:.+\\.)?evil\\.com$", "a".repeat(300));
    let mut rule = wire(&app_bound("010-long", "allow"));
    rule["operator"]["operands"][1] =
        json!({ "type": "regexp", "operand": "dest.host", "data": pattern });
    let items = previewed(document(vec![rule]), &synced(Vec::new()));
    assert_eq!(items[0].kind, ImportKind::Add, "{items:?}");
    let shown = &items[0].conditions[1];
    assert!(shown.ends_with("evil\\.com$"), "{shown}");
    assert!(shown.contains(&pattern), "{shown}");
}

// --- Fixtures -------------------------------------------------------------

fn fixtures(kind: &str) -> Vec<(String, String)> {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/rules-io/"
    );
    let mut files: Vec<_> = std::fs::read_dir(format!("{dir}{kind}"))
        .expect("fixture dir")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no {kind} fixtures");
    files
}

#[test]
fn every_valid_fixture_parses_and_previews_without_refusals() {
    for (name, text) in fixtures("valid") {
        let doc = parse_document_text(&text).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let items = preview(&doc, &synced(Vec::new())).unwrap().items;
        assert!(
            items.iter().all(|i| i.kind == ImportKind::Add),
            "{name}: {items:?}"
        );
    }
}

/// An invalid fixture is named `<reason>--<description>.json`; `<reason>`
/// is a [`DocumentError`] variant or, for a document that parses, `refused`
/// (every rule in it is refused).
#[test]
fn every_invalid_fixture_fails_with_its_named_reason() {
    for (name, text) in fixtures("invalid") {
        let reason = name.split("--").next().unwrap();
        let parsed = parse_document_text(&text);
        let got = match &parsed {
            Err(error) => format!("{error:?}"),
            Ok(doc) => {
                let items = preview(doc, &synced(Vec::new())).unwrap().items;
                assert!(!items.is_empty(), "{name}");
                if items.iter().all(|i| i.kind == ImportKind::Refused) {
                    "refused".to_string()
                } else {
                    format!("accepted: {items:?}")
                }
            }
        };
        assert_eq!(got, reason, "{name}");
    }
}
