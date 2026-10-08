//! Tests for [`super`]: the GUI-sourced shapes it refuses, and every shape
//! the bridge itself builds, which must pass.

use super::*;
use crate::cache::connections::Verdict;
use crate::rule_wire::{rule_from_wire, rule_to_wire};
use crate::translator::verdict::{once_rule, verdict_to_rule};
use crate::ws_messages::{VerdictDuration, VerdictScope};
use serde_json::{json, Value};
use snitchwatch_proto::protocol::Connection;

fn leaf(r#type: &str, operand: &str, data: &str) -> Value {
    json!({ "type": r#type, "operand": operand, "data": data, "sensitive": false })
}

fn list(members: Vec<Value>) -> Value {
    json!({ "type": "list", "operands": members })
}

fn wire_rule(operator: Value) -> Value {
    json!({
        "name": "899-test",
        "enabled": true,
        "action": "deny",
        "duration": "always",
        "operator": operator,
    })
}

fn refused(operator: Value) -> bool {
    rule_from_wire(&wire_rule(operator)).is_err()
}

fn process() -> Value {
    leaf("simple", "process.path", "/usr/bin/curl")
}

// --- GUI-sourced shapes that crash, fail open, or match everything ----

#[test]
fn network_type_with_a_string_operand_is_refused() {
    // cmpNetwork does `value.(net.IP)`; dest.ip passes a string → panic.
    assert!(refused(leaf("network", "dest.ip", "10.0.0.0/8")));
}

#[test]
fn simple_type_with_a_network_operand_is_refused() {
    // simpleCmp does `v.(string)`; dest.network passes a net.IP → panic.
    assert!(refused(leaf("simple", "dest.network", "10.0.0.0/8")));
}

#[test]
fn regexp_type_with_a_network_operand_is_refused() {
    // reCmp sees a net.IP and never matches: a rule that silently does
    // nothing.
    assert!(refused(leaf("regexp", "source.network", "^10\\.")));
}

#[test]
fn a_leaf_with_the_list_operand_is_refused() {
    // Match checks the operand `list` before the type; a non-list type's
    // members are dropped by Deserialize, and an empty list matches all.
    assert!(refused(leaf("simple", "list", "")));
}

#[test]
fn an_empty_list_is_refused_in_both_wire_spellings() {
    assert!(refused(list(Vec::new())), "operands: []");
    assert!(
        refused(leaf("list", "list", "")),
        "list type with no operands array"
    );
}

#[test]
fn a_nested_list_is_refused() {
    // Deserialize copies members one level deep: the inner list reaches
    // the daemon empty and matches every connection.
    let inner = list(vec![leaf("simple", "dest.host", "example.com")]);
    assert!(refused(list(vec![process(), inner])));
}

#[test]
fn a_list_member_with_the_list_operand_is_refused() {
    assert!(refused(list(vec![process(), leaf("simple", "list", "")])));
}

#[test]
fn a_list_of_more_than_64_members_is_refused() {
    let members = vec![leaf("simple", "dest.host", "example.com"); 65];
    assert!(refused(list(members)));
    let members = vec![leaf("simple", "dest.host", "example.com"); 64];
    assert!(!refused(list(members)), "64 is the limit, not over it");
}

#[test]
fn a_non_list_type_with_members_is_refused() {
    // operator_from_wire picks the list branch from `operands`, whatever
    // the type; Deserialize then drops the members of a non-list type.
    let op = json!({ "type": "simple", "operands": [process()] });
    assert!(refused(op));
}

#[test]
fn the_true_operand_with_a_non_simple_type_is_refused() {
    assert!(refused(leaf("regexp", "true", "^x$")));
    assert!(refused(leaf("network", "true", "10.0.0.0/8")));
}

#[test]
fn blocklist_operands_and_the_lists_type_are_refused() {
    // A legacy daemon reads whatever directory `data` names, as root.
    assert!(refused(leaf("lists", "lists.domains", "/etc")));
    assert!(refused(leaf("simple", "lists.domains", "/etc")));
    assert!(refused(leaf("simple", "lists.nets", "/etc")));
    assert!(refused(leaf("regexp", "lists.domains_regexp", "/etc")));
    assert!(refused(leaf("lists", "dest.host", "/etc")));
}

#[test]
fn unknown_types_and_operands_are_refused() {
    assert!(refused(leaf("range", "dest.port", "1-2")));
    assert!(refused(leaf("complex", "dest.host", "x")));
    assert!(refused(leaf("Simple", "dest.host", "x")));
    assert!(refused(leaf("simple", "dest.hostname", "x")));
    assert!(refused(leaf("simple", "Dest.Host", "x")));
    assert!(refused(leaf("simple", "process.env.", "x")));
    assert!(refused(leaf("simple", "process.env.A B", "x")));
}

#[test]
fn a_regexp_that_does_not_compile_is_refused() {
    assert!(refused(leaf("regexp", "dest.host", "(unclosed")));
    // The daemon lowercases an insensitive pattern before compiling it:
    // `[Z-a]` is a valid range, `[z-a]` is not.
    assert!(refused(leaf("regexp", "dest.host", "[Z-a]")));
    let sensitive =
        json!({ "type": "regexp", "operand": "dest.host", "data": "[Z-a]", "sensitive": true });
    assert!(!refused(sensitive));
}

#[test]
fn network_data_that_is_not_a_cidr_is_refused() {
    for data in [
        "",
        "LAN",
        "10.0.0.0",
        "10.0.0.0/33",
        "10.0.0.0/+8",
        "10.0.0.0/",
        "::/129",
        "fe80::1%eth0/64",
        "10.0.0.0/8/8",
    ] {
        assert!(refused(leaf("network", "dest.network", data)), "{data:?}");
    }
    assert!(!refused(leaf("network", "dest.network", "10.0.0.0/8")));
    assert!(!refused(leaf("network", "source.network", "fc00::/7")));
}

#[test]
fn over_long_data_is_refused() {
    let long = "a".repeat(crate::cache::rules::MAX_RULE_FIELD_BYTES + 1);
    assert!(refused(leaf("simple", "dest.host", &long)));
}

#[test]
fn a_disabled_rule_is_checked_too() {
    // The daemon only compiles enabled rules, so a disabled bad shape is
    // stored unchecked and panics once something enables it.
    let mut rule = wire_rule(leaf("network", "dest.ip", "10.0.0.0/8"));
    rule["enabled"] = json!(false);
    assert!(rule_from_wire(&rule).is_err());
}

// --- Direct checks on the proto operator ------------------------------

fn op(r#type: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: r#type.to_string(),
        operand: operand.to_string(),
        data: data.to_string(),
        ..Default::default()
    }
}

fn list_op(operand: &str, members: Vec<Operator>) -> Operator {
    Operator {
        r#type: "list".to_string(),
        operand: operand.to_string(),
        list: members,
        ..Default::default()
    }
}

#[test]
fn a_list_operand_must_be_list_or_empty() {
    let host = op("simple", "dest.host", "example.com");
    assert!(validate_operator(&list_op("list", vec![host.clone()])).is_ok());
    assert!(validate_operator(&list_op("", vec![host.clone()])).is_ok());
    assert!(validate_operator(&list_op("dest.host", vec![host])).is_err());
}

#[test]
fn a_leaf_with_members_is_refused_even_with_a_valid_operand() {
    // The wire can't express this (an `operands` array drops the
    // operand), but a daemon rule or a later import can: Deserialize
    // drops the members of a non-list type.
    let leaf = Operator {
        list: vec![op("simple", "dest.host", "example.com")],
        ..op("simple", "process.path", "/usr/bin/curl")
    };
    assert!(validate_operator(&leaf).is_err());
}

#[test]
fn list_members_may_not_be_lists_or_blocklists() {
    let host = op("simple", "dest.host", "example.com");
    for member in [
        list_op("list", vec![host.clone()]),
        op("list", "", ""),
        op("lists", "lists.domains", "/etc"),
        op("simple", "list", ""),
    ] {
        assert!(
            validate_operator(&list_op("list", vec![host.clone(), member.clone()])).is_err(),
            "{member:?}"
        );
    }
}

#[test]
fn errors_never_echo_the_operand_or_data() {
    const MARK: &str = "MARKER";
    let bad = [
        op("simple", "dest.hostMARKER", "x"),
        op("MARKER", "dest.host", "x"),
        op("regexp", "dest.host", "(MARKER"),
        op("network", "dest.network", "MARKER"),
        op("network", "dest.network", "10.0.0.0/MARKER"),
        op("network", "dest.ip", "MARKER"),
        op("simple", "process.env.MARK ER", "x"),
        op("lists", "lists.domains", "/MARKER"),
        op("simple", "dest.host", &MARK.repeat(4000)),
        list_op("MARKER", vec![op("simple", "dest.host", "x")]),
        list_op("list", vec![op("regexp", "dest.host", "(MARKER")]),
    ];
    for operator in bad {
        let err = validate_operator(&operator).unwrap_err();
        assert!(!err.contains(MARK), "{err}");
    }
}

#[test]
fn every_daemon_leaf_operand_is_accepted_with_simple() {
    for operand in [
        "true",
        "process.id",
        "process.path",
        "process.parent.path",
        "process.command",
        "process.env.HOME",
        "process.env._X1",
        "user.id",
        "user.name",
        "source.ip",
        "source.port",
        "dest.ip",
        "dest.host",
        "dest.port",
        "protocol",
        "iface.in",
        "iface.out",
    ] {
        assert!(
            validate_operator(&op("simple", operand, "x")).is_ok(),
            "{operand}"
        );
    }
}

// --- Every shape the bridge itself produces must pass -----------------

fn connections() -> Vec<Connection> {
    let base = Connection {
        protocol: "tcp".into(),
        dst_ip: "140.82.121.4".into(),
        dst_host: "api.github.com".into(),
        dst_port: 443,
        process_path: "/usr/bin/curl".into(),
        ..Default::default()
    };
    vec![
        base.clone(),
        Connection {
            process_path: String::new(),
            ..base.clone()
        },
        Connection {
            dst_host: String::new(),
            ..base.clone()
        },
        Connection {
            dst_host: "140.82.121.4".into(),
            ..base.clone()
        },
        Connection {
            dst_host: "USER.Docs.GitHub.COM".into(),
            ..base.clone()
        },
        Connection {
            dst_host: "github.com".into(),
            ..base.clone()
        },
        Connection {
            dst_host: String::new(),
            dst_ip: "2001:db8::1".into(),
            process_path: String::new(),
            ..base
        },
    ]
}

#[test]
fn every_verdict_rule_the_bridge_builds_passes_directly_and_after_the_wire() {
    let mut seen_types = std::collections::BTreeSet::new();
    for conn in connections() {
        for scope in [
            VerdictScope::ThisHost,
            VerdictScope::AnyHostOnDomain,
            VerdictScope::AnyHost,
        ] {
            for verdict in [Verdict::Allow, Verdict::Deny] {
                // A refused remembered verdict is answered with this
                // once-only rule instead (issue #44).
                let rule = verdict_to_rule(verdict, VerdictDuration::Always, scope, &conn, 0)
                    .unwrap_or_else(|_| once_rule(verdict, scope, &conn, 0));
                let operator = rule.operator.as_ref().unwrap();
                seen_types.insert(operator.r#type.clone());
                seen_types.extend(operator.list.iter().map(|m| m.r#type.clone()));
                assert_eq!(validate_operator(operator), Ok(()), "{rule:?}");
                assert_eq!(read_only_reason(&rule), None, "{rule:?}");
                let back = rule_from_wire(&rule_to_wire(&rule));
                assert!(back.is_ok(), "{rule:?}: {back:?}");
            }
        }
    }
    let expected = ["list", "regexp", "simple"].map(String::from);
    assert_eq!(seen_types, expected.into_iter().collect());
}

#[test]
fn other_shapes_a_gui_legitimately_sends_pass() {
    let ok = [
        op("simple", "true", ""),
        op("regexp", "dest.host", r"^(?:[^.]+\.)*example\.com$"),
        op("simple", "dest.host", ""),
        op("network", "dest.network", "192.168.1.5/24"),
        op("network", "source.network", "::ffff:10.0.0.0/104"),
        list_op(
            "list",
            vec![
                Operator {
                    sensitive: true,
                    ..op("simple", "process.path", "/usr/bin/curl")
                },
                op("regexp", "dest.host", "^example\\.com$"),
                op("network", "dest.network", "10.0.0.0/8"),
                op("simple", "true", ""),
            ],
        ),
    ];
    for operator in ok {
        assert_eq!(validate_operator(&operator), Ok(()), "{operator:?}");
    }
}

// --- Daemon rules with a refused shape are listed read-only -----------

fn daemon_rule(name: &str, operator: Option<Operator>) -> Rule {
    Rule {
        name: name.to_string(),
        action: "deny".into(),
        duration: "always".into(),
        operator,
        ..Default::default()
    }
}

#[test]
fn a_daemon_rule_with_a_refused_shape_is_read_only_with_a_reason() {
    for operator in [
        Some(op("network", "dest.network", "LAN")),
        Some(op("lists", "lists.domains", "/var/lib/blocklist")),
        Some(op("network", "dest.ip", "10.0.0.0/8")),
        None,
    ] {
        let rule = daemon_rule("from-the-stock-ui", operator);
        assert_eq!(read_only_reason(&rule), Some(SHAPE_READ_ONLY_REASON));
        let wire = rule_to_wire(&rule);
        assert_eq!(wire["readOnlyReason"], SHAPE_READ_ONLY_REASON, "{rule:?}");
        assert!(rule_from_wire(&wire).is_err(), "never sent back: {rule:?}");
    }
    assert!(!SHAPE_READ_ONLY_REASON.is_empty());
    assert_ne!(SHAPE_READ_ONLY_REASON, crate::rule_wire::READ_ONLY_REASON);
}

#[test]
fn the_name_reason_wins_when_name_and_shape_are_both_refused() {
    let rule = daemon_rule("a/b", Some(op("network", "dest.ip", "10.0.0.0/8")));
    assert_eq!(
        read_only_reason(&rule),
        Some(crate::rule_wire::READ_ONLY_REASON)
    );
}

#[test]
fn an_editable_daemon_rule_has_no_read_only_reason() {
    let rule = daemon_rule("899-ok", Some(op("simple", "dest.host", "example.com")));
    assert_eq!(read_only_reason(&rule), None);
    assert!(rule_to_wire(&rule)["readOnlyReason"].is_null());
}

// --- Follow-ups: remaining match-all and dead shapes ------------------

fn hash(operand: &str) -> Value {
    leaf("simple", operand, "0123456789abcdef0123456789abcdef")
}

#[test]
fn a_hash_leaf_is_refused_whatever_its_type() {
    // Match returns true for process.hash.* when checksums are off (the
    // default), and hashCmp returns true for an empty hash.
    for operand in ["process.hash.md5", "process.hash.sha1"] {
        assert!(refused(hash(operand)), "{operand}");
        assert!(refused(leaf("regexp", operand, "^0")), "{operand}");
    }
}

#[test]
fn a_list_constrained_only_by_hashes_is_refused() {
    let always = leaf("simple", "true", "");
    assert!(refused(list(vec![hash("process.hash.md5")])));
    assert!(refused(list(vec![
        always.clone(),
        hash("process.hash.sha1"),
        hash("process.hash.md5"),
    ])));
    // Another member still constrains the list when checksums are off.
    assert!(!refused(list(vec![process(), hash("process.hash.md5")])));
    assert!(
        !refused(list(vec![always.clone(), always])),
        "explicit, like `true`"
    );
}

#[test]
fn a_regexp_that_matches_every_host_is_refused() {
    // Unanchored: an empty match fits every dest.host.
    assert!(refused(leaf("regexp", "dest.host", "a*")));
    assert!(refused(leaf("regexp", "dest.host", "^")));
    assert!(refused(list(vec![
        process(),
        leaf("regexp", "dest.host", ".*")
    ])));
    assert!(!refused(leaf("regexp", "dest.host", "^a*$")));
}

#[test]
fn an_empty_or_missing_regexp_is_refused() {
    assert!(refused(leaf("regexp", "dest.host", "")));
    assert!(refused(json!({ "type": "regexp", "operand": "dest.host" })));
    assert!(refused(list(vec![
        process(),
        leaf("regexp", "dest.host", "")
    ])));
    assert!(!refused(leaf("regexp", "dest.host", "^$")));
}

#[test]
fn a_case_insensitive_pattern_that_lowercasing_changes_is_refused() {
    // `^\D*$` lowercased is `^\d*$`: it matches the empty dest.host of
    // every bare-IP connection.
    assert!(refused(leaf("regexp", "dest.host", r"^\D*$")));
    let sensitive =
        json!({ "type": "regexp", "operand": "dest.host", "data": r"^\D+$", "sensitive": true });
    assert!(!refused(sensitive));
    assert!(
        !refused(leaf("regexp", "dest.host", r"^\\D$")),
        "a literal backslash"
    );
}

#[test]
fn regexp_dialect_differences_are_refused() {
    assert!(refused(leaf("regexp", "dest.host", "a{1001}")));
    assert!(refused(leaf("regexp", "dest.host", "[a-z&&b]")));
    assert!(refused(leaf("regexp", "dest.host", "(?x)a")));
}

#[test]
fn regexp_with_user_name_is_refused() {
    // Compile maps a user name to its uid only for `simple`; a regexp
    // would be compared against the uid string and never match a name.
    assert!(refused(leaf("regexp", "user.name", "^root$")));
    assert!(!refused(leaf("simple", "user.name", "root")));
    assert!(!refused(leaf("regexp", "user.id", "^0$")));
}

#[test]
fn a_user_name_holding_a_uid_is_refused_alone_and_in_a_list() {
    // The daemon reports a loaded user.name rule with the uid `Compile`
    // wrote over the name; sent back, it is looked up as a name and fails.
    assert!(refused(leaf("simple", "user.name", "987")));
    assert!(refused(leaf("simple", "user.name", "0")));
    assert!(refused(list(vec![
        process(),
        leaf("simple", "user.name", "987")
    ])));
    let err = validate_operator(&op("simple", "user.name", "987")).unwrap_err();
    assert!(!err.contains("987"), "errors never echo data: {err}");

    // Names, and uids under user.id, still pass.
    assert!(!refused(leaf("simple", "user.name", "snitchwatch")));
    assert!(!refused(leaf("simple", "user.name", "user1")));
    assert!(!refused(leaf("simple", "user.id", "987")));
}

/// The rules Snitchwatch ships are listed read-only with fixed text and
/// can't be deleted, whatever their conditions.
#[test]
fn a_packaged_rule_is_read_only_with_fixed_text_and_not_deletable() {
    use crate::rule_name::PACKAGED_FETCH_RULE_NAME;
    for (name, reason) in [
        (PACKAGED_FETCH_RULE_NAME, PACKAGED_FETCH_RULE_REASON),
        ("000-snitchwatch-other", PACKAGED_RULE_REASON),
    ] {
        let rule = daemon_rule(name, Some(op("simple", "dest.host", "example.com")));
        assert_eq!(read_only_reason(&rule), Some(reason), "{name}");
        assert!(!deletable(&rule), "{name}");
        let wire = rule_to_wire(&rule);
        assert_eq!(wire["readOnlyReason"], reason);
        assert_eq!(wire["deletable"], false);
    }
    assert!(!PACKAGED_RULE_REASON.contains("000-"), "fixed text only");
}

#[test]
fn a_daemon_user_name_rule_reported_with_its_uid_is_read_only_but_deletable() {
    let rule = daemon_rule("000-x", Some(op("simple", "user.name", "987")));
    assert_eq!(read_only_reason(&rule), Some(SHAPE_READ_ONLY_REASON));
    assert!(deletable(&rule));
}

#[test]
fn a_proto_list_over_the_limit_is_refused() {
    // The wire path stops this in operator_from_wire; a proto caller
    // (a daemon rule, a later import) reaches the validator.
    let members = vec![op("simple", "dest.host", "example.com"); MAX_OPERATOR_LIST_LEN + 1];
    assert!(validate_operator(&list_op("list", members)).is_err());
}

#[test]
fn hash_operands_are_accepted_next_to_a_constraining_member() {
    for operand in ["process.hash.md5", "process.hash.sha1"] {
        let members = vec![
            op("simple", "process.path", "/usr/bin/curl"),
            op("simple", operand, "x"),
        ];
        assert_eq!(
            validate_operator(&list_op("list", members)),
            Ok(()),
            "{operand}"
        );
    }
}

#[test]
fn a_proto_hash_leaf_and_hash_only_list_are_refused() {
    assert!(validate_operator(&op("simple", "process.hash.md5", "x")).is_err());
    let members = vec![
        op("simple", "true", ""),
        op("simple", "process.hash.sha1", "x"),
    ];
    assert!(validate_operator(&list_op("list", members)).is_err());
}

// --- Blocklist rules are managed on the Blocklists page (issue #45) -------

#[test]
fn a_blocklist_rule_is_read_only_and_not_deletable_from_the_rules_page() {
    for name in [
        "z00-blocklist:ads-0123456789abcdef:domains",
        "z00-blocklist:x:ips",
        "900-blocklist:ads:0001-x.example",
    ] {
        for operator in [
            Some(op(
                "lists",
                "lists.domains",
                "/var/lib/snitchwatch/blocklists/x",
            )),
            Some(op("simple", "dest.host", "example.com")),
        ] {
            let rule = daemon_rule(name, operator);
            assert_eq!(read_only_reason(&rule), Some(BLOCKLIST_MANAGED_REASON));
            assert!(!deletable(&rule), "{name}");
            let wire = rule_to_wire(&rule);
            assert_eq!(wire["readOnlyReason"], BLOCKLIST_MANAGED_REASON);
            assert_eq!(wire["deletable"], false);
        }
    }
    assert!(BLOCKLIST_MANAGED_REASON.starts_with("Managed on the Blocklists page"));
    assert!(!BLOCKLIST_MANAGED_REASON.contains("can't"));
    let look_alike = daemon_rule(
        "z00-blocklisted",
        Some(op("simple", "dest.host", "example.com")),
    );
    assert_eq!(read_only_reason(&look_alike), None);
    assert!(deletable(&look_alike));
}

// --- Delete stays available for a rule refused only for its shape ---------

#[test]
fn a_shape_only_refusal_stays_deletable_but_a_bad_name_does_not() {
    let shape = daemon_rule("899-lan", Some(op("network", "dest.network", "LAN")));
    let wire = rule_to_wire(&shape);
    assert_eq!(wire["readOnlyReason"], SHAPE_READ_ONLY_REASON);
    assert_eq!(wire["deletable"], true);
    assert!(SHAPE_READ_ONLY_REASON.contains("can still delete"));

    let bad_name = daemon_rule("a/b", Some(op("simple", "dest.host", "example.com")));
    assert_eq!(rule_to_wire(&bad_name)["deletable"], false);

    let editable = daemon_rule("899-ok", Some(op("simple", "dest.host", "example.com")));
    assert_eq!(rule_to_wire(&editable)["deletable"], true);
}

#[test]
fn a_curated_default_rule_is_read_only_and_not_deletable() {
    let curated = daemon_rule(
        "snitchwatch-default-steam",
        Some(op("simple", "dest.host", "steam.example")),
    );
    assert_eq!(read_only_reason(&curated), Some(CURATED_MANAGED_REASON));
    assert!(!deletable(&curated));
}
