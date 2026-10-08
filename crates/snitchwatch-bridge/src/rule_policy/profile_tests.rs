//! Tests for [`super::profile`]: the `Import` profile's own refusals, and
//! that every `validate_operator` refusal reaches the import path with
//! `validate_operator`'s reason (so dropping the call fails a test even where
//! the profile's vocabulary would also refuse the shape).

use super::profile::*;
use super::{binds_to_programs, validate_operator};
use snitchwatch_proto::protocol::{Operator, Rule};

fn leaf(r#type: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: r#type.into(),
        operand: operand.into(),
        data: data.into(),
        ..Default::default()
    }
}

fn list(members: Vec<Operator>) -> Operator {
    Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: members,
        ..Default::default()
    }
}

fn path_leaf() -> Operator {
    leaf("simple", "process.path", "/usr/bin/curl")
}

fn rule(operator: Operator) -> Rule {
    Rule {
        name: "899-imported".into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(operator),
        ..Default::default()
    }
}

fn import(rule: &Rule) -> Result<(), Vec<RuleProblem>> {
    validate_user_rule(rule, PolicyProfile::Import)
}

fn reasons(rule: &Rule) -> Vec<String> {
    import(rule)
        .expect_err("expected the import profile to refuse this rule")
        .into_iter()
        .map(|p| p.reason)
        .collect()
}

fn refused_with(rule: &Rule, reason: &str) {
    let got = reasons(rule);
    assert!(
        got.iter().any(|r| r == reason),
        "expected reason {reason:?}, got {got:?}"
    );
}

fn operator_reason(op: &Operator) -> String {
    validate_operator(op).expect_err("validate_operator should refuse this")
}

// --- validate_operator's refusals reach the import path -------------------

#[test]
fn every_pairing_refusal_reaches_the_import_path_with_its_own_reason() {
    let many = vec![path_leaf(); 65];
    let cases = [
        (
            "network + dest.ip",
            leaf("network", "dest.ip", "10.0.0.0/8"),
        ),
        (
            "simple + dest.network",
            leaf("simple", "dest.network", "10.0.0.0/8"),
        ),
        (
            "regexp + source.network",
            leaf("regexp", "source.network", "^10"),
        ),
        ("list operand on simple", leaf("simple", "list", "x")),
        ("empty list", list(Vec::new())),
        ("65-member list", list(many)),
        ("nested list", list(vec![list(vec![path_leaf()])])),
        ("true as regexp", leaf("regexp", "true", "x")),
        ("lists.domains", leaf("lists", "lists.domains", "/etc/x")),
    ];
    for (case, op) in cases {
        let expected = operator_reason(&op);
        let got = reasons(&rule(op));
        assert!(got.contains(&expected), "{case}: {got:?}");
    }
}

#[test]
fn lists_operands_are_refused_at_any_depth_and_as_a_type() {
    let top = leaf("lists", "lists.domains", "/var/lib/x");
    refused_with(&rule(top.clone()), &operator_reason(&top));

    // Depth 3: a list inside a list inside a list.
    let deep = list(vec![list(vec![list(vec![leaf(
        "lists",
        "lists.nets",
        "/var/lib/x",
    )])])]);
    refused_with(&rule(deep.clone()), &operator_reason(&deep));

    let as_type = leaf("lists", "dest.host", "x");
    refused_with(&rule(as_type.clone()), &operator_reason(&as_type));
}

#[test]
fn unknown_operands_bad_regexps_and_bad_cidrs_are_refused() {
    for op in [
        leaf("simple", "dest.hostname", "x"),
        leaf("regexp", "dest.host", "(unclosed"),
        leaf("network", "dest.network", "10.0.0.0/33"),
    ] {
        refused_with(&rule(op.clone()), &operator_reason(&op));
    }
}

/// The plan accepted `LAN`/`MULTICAST`; the merged `validate_operator`
/// refuses every network alias (the bridge can't see the daemon host's
/// alias file), and `DaemonCommands::send` refuses what it refuses, so an
/// import can't carry one either.
#[test]
fn network_aliases_are_refused_on_import() {
    for alias in ["LAN", "MULTICAST", "LAN2"] {
        let op = leaf("network", "dest.network", alias);
        refused_with(&rule(op.clone()), &operator_reason(&op));
    }
}

// --- The Import profile's own refusals -----------------------------------

#[test]
fn a_hash_condition_is_refused_even_next_to_a_narrowing_one() {
    let alone = leaf(
        "simple",
        "process.hash.md5",
        "d41d8cd98f00b204e9800998ecf8427e",
    );
    refused_with(&rule(alone.clone()), &operator_reason(&alone));

    // validate_operator accepts this list; only the import profile refuses.
    let narrowed = list(vec![
        path_leaf(),
        leaf(
            "simple",
            "process.hash.sha1",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        ),
    ]);
    assert_eq!(validate_operator(&narrowed), Ok(()));
    refused_with(&rule(narrowed), HASH_REFUSED);
}

#[test]
fn match_all_rules_are_refused() {
    let top = leaf("simple", "true", "");
    assert_eq!(validate_operator(&top), Ok(()));
    refused_with(&rule(top), MATCHES_EVERYTHING);

    let all_true = list(vec![leaf("simple", "true", ""), leaf("simple", "true", "")]);
    assert_eq!(validate_operator(&all_true), Ok(()));
    refused_with(&rule(all_true), MATCHES_EVERYTHING);

    let narrowed = list(vec![leaf("simple", "true", ""), path_leaf()]);
    assert_eq!(import(&rule(narrowed)), Ok(()));
}

#[test]
fn only_permanent_durations_are_imported() {
    for duration in ["once", "5m", "", "until-restart", "Always"] {
        let mut r = rule(path_leaf());
        r.duration = duration.into();
        refused_with(&r, DURATION_REFUSED);
    }
    for duration in ["always", "until restart"] {
        let mut r = rule(path_leaf());
        r.duration = duration.into();
        assert_eq!(import(&r), Ok(()), "{duration}");
    }
}

#[test]
fn only_allow_deny_and_reject_actions_are_imported() {
    for action in ["", "drop", "Allow", "accept"] {
        let mut r = rule(path_leaf());
        r.action = action.into();
        refused_with(&r, ACTION_REFUSED);
    }
    for action in ["allow", "deny", "reject"] {
        let mut r = rule(path_leaf());
        r.action = action.into();
        assert_eq!(import(&r), Ok(()), "{action}");
    }
}

#[test]
fn ports_and_ids_must_be_decimal_and_in_range() {
    for (operand, data, reason) in [
        ("dest.port", "70000", PORT_REFUSED),
        ("source.port", "-1", PORT_REFUSED),
        ("dest.port", "0x50", PORT_REFUSED),
        ("dest.port", "", EMPTY_VALUE_REFUSED),
        ("process.id", "12a", ID_REFUSED),
        ("user.id", "-5", ID_REFUSED),
        ("user.id", "99999999999", ID_REFUSED),
    ] {
        let r = rule(list(vec![path_leaf(), leaf("simple", operand, data)]));
        let problems = import(&r).expect_err(operand);
        assert!(
            problems
                .iter()
                .any(|p| p.reason == reason && p.path == "operator.list[1].data"),
            "{operand}={data}: {problems:?}"
        );
    }
    for (operand, data) in [
        ("dest.port", "0"),
        ("dest.port", "65535"),
        ("process.id", "1"),
        ("user.id", "1000"),
    ] {
        let r = rule(list(vec![path_leaf(), leaf("simple", operand, data)]));
        assert_eq!(import(&r), Ok(()), "{operand}={data}");
    }
    // A regexp on a port is a pattern, not a number.
    let pattern = rule(list(vec![
        path_leaf(),
        leaf("regexp", "dest.port", "^44[3]$"),
    ]));
    assert_eq!(import(&pattern), Ok(()));
}

#[test]
fn bridge_owned_and_unsafe_names_are_refused() {
    for (name, reason) in [
        ("z00-blocklist:ads:domains", BLOCKLIST_NAME_REFUSED),
        ("900-blocklist:ads:domains", BLOCKLIST_NAME_REFUSED),
        ("snitchwatch-default-steam", CURATED_NAME_REFUSED),
        ("000-snitchwatch-fetch", PACKAGED_NAME_REFUSED),
    ] {
        let mut r = rule(path_leaf());
        r.name = name.into();
        refused_with(&r, reason);
    }
    for name in ["a/b", "", "..", "x\u{202e}y"] {
        let mut r = rule(path_leaf());
        r.name = name.into();
        let problems = import(&r).expect_err(name);
        assert!(problems.iter().any(|p| p.path == "name"), "{name:?}");
    }
}

#[test]
fn over_cap_input_is_refused() {
    let mut long = rule(path_leaf());
    long.description = "d".repeat(crate::cache::rules::MAX_RULE_FIELD_BYTES + 1);
    refused_with(&long, TOO_LARGE);
    let mut at_cap = rule(path_leaf());
    at_cap.description = "d".repeat(crate::cache::rules::MAX_RULE_FIELD_BYTES);
    assert_eq!(import(&at_cap), Ok(()));

    let wide = list(vec![
        path_leaf();
        crate::cache::rules::MAX_OPERATOR_LIST_LEN + 1
    ]);
    refused_with(&rule(wide.clone()), &operator_reason(&wide));

    let deep = (1..5).fold(path_leaf(), |inner, _| list(vec![inner]));
    refused_with(&rule(deep.clone()), &operator_reason(&deep));
}

#[test]
fn a_rule_without_conditions_is_refused() {
    let mut r = rule(path_leaf());
    r.operator = None;
    refused_with(&r, NO_CONDITIONS);
}

// --- What the Import profile accepts --------------------------------------

#[test]
fn each_allowed_operand_is_accepted_once() {
    let members = [
        leaf("simple", "true", ""),
        leaf("simple", "process.path", "/usr/bin/curl"),
        leaf("regexp", "process.parent.path", "^/usr/bin/"),
        leaf("simple", "process.command", "curl https://example.com"),
        leaf("simple", "process.id", "4242"),
        leaf("simple", "process.env.HOME", "/home/me"),
        leaf("simple", "user.id", "1000"),
        leaf("simple", "user.name", "me"),
        leaf("simple", "source.ip", "192.168.1.2"),
        leaf("simple", "source.port", "5353"),
        leaf("network", "source.network", "192.168.0.0/16"),
        leaf("simple", "dest.ip", "93.184.216.34"),
        leaf("regexp", "dest.host", "^(?:[^.]+\\.)*example\\.com$"),
        leaf("simple", "dest.port", "443"),
        leaf("network", "dest.network", "fc00::/7"),
        leaf("simple", "protocol", "tcp"),
        leaf("simple", "iface.in", "eth0"),
        leaf("simple", "iface.out", "wlan0"),
    ];
    for member in members {
        let r = rule(list(vec![path_leaf(), member.clone()]));
        assert_eq!(import(&r), Ok(()), "{}", member.operand);
    }
    assert_eq!(import(&rule(path_leaf())), Ok(()));
}

#[test]
fn problems_never_echo_the_rule_text() {
    let mut r = rule(list(vec![
        leaf("simple", "dest.port", "MARKER"),
        leaf("regexp", "dest.host", "(MARKER"),
        leaf("simple", "process.hash.md5", "MARKER"),
        leaf("simple", "process.env.MARKER", "MARKER"),
    ]));
    r.name = "z00-blocklist:MARKER/x".into();
    r.action = "MARKER".into();
    r.duration = "MARKER".into();
    r.description = format!("MARKER{}", "d".repeat(20_000));
    let problems = import(&r).expect_err("refused");
    assert!(!problems.is_empty());
    for problem in problems {
        let text = format!("{} {}", problem.path, problem.reason).to_lowercase();
        assert!(!text.contains("marker"), "{problem:?}");
    }
}

// --- Match-all shapes beyond `true` (P2.7 review H1) ----------------------

#[test]
fn an_empty_simple_value_is_refused_except_for_dest_host() {
    for operand in [
        "process.env.ZZZ",
        "process.path",
        "dest.ip",
        "user.id",
        "protocol",
    ] {
        let r = rule(list(vec![
            leaf("simple", "dest.port", "443"),
            leaf("simple", operand, ""),
        ]));
        let problems = import(&r).expect_err(operand);
        assert!(
            problems
                .iter()
                .any(|p| p.reason == EMPTY_VALUE_REFUSED && p.path == "operator.list[1].data"),
            "{operand}: {problems:?}"
        );
    }
    // An empty dest.host is the daemon's match for a connection with no
    // host name: it narrows.
    assert_eq!(import(&rule(leaf("simple", "dest.host", ""))), Ok(()));
}

#[test]
fn rules_whose_conditions_all_match_everything_are_refused() {
    for op in [
        leaf("regexp", "process.path", "/"),
        leaf("regexp", "process.parent.path", "^/"),
        leaf("regexp", "dest.host", ".+"),
        leaf("regexp", "process.command", "[a-z]"),
        leaf("network", "dest.network", "0.0.0.0/0"),
        leaf("network", "source.network", "::/0"),
        list(vec![
            leaf("regexp", "process.path", "/"),
            leaf("network", "dest.network", "0.0.0.0/0"),
        ]),
    ] {
        assert_eq!(validate_operator(&op), Ok(()), "{op:?}");
        refused_with(&rule(op), MATCHES_EVERYTHING);
    }
}

#[test]
fn a_match_everything_condition_next_to_a_narrowing_one_is_accepted() {
    for op in [
        list(vec![path_leaf(), leaf("regexp", "dest.host", ".+")]),
        list(vec![
            path_leaf(),
            leaf("network", "dest.network", "0.0.0.0/0"),
        ]),
        list(vec![
            leaf("regexp", "process.path", "/"),
            leaf("simple", "dest.port", "443"),
        ]),
        leaf("regexp", "process.path", "^/usr/bin/curl$"),
        leaf("regexp", "dest.host", "^(?:[^.]+\\.)*example\\.com$"),
    ] {
        assert_eq!(import(&rule(op.clone())), Ok(()), "{op:?}");
    }
}

#[test]
fn only_a_simple_path_command_or_id_ties_a_rule_to_programs() {
    for (op, bound) in [
        (path_leaf(), true),
        (leaf("simple", "process.command", "curl x"), true),
        (leaf("simple", "process.id", "42"), true),
        (leaf("regexp", "process.path", "^/usr/bin/curl$"), false),
        (
            leaf("simple", "process.parent.path", "/usr/lib/systemd/systemd"),
            false,
        ),
        (leaf("simple", "process.env.HOME", "/root"), false),
        (leaf("simple", "dest.host", "example.com"), false),
        (
            list(vec![leaf("simple", "dest.port", "443"), path_leaf()]),
            true,
        ),
    ] {
        assert_eq!(binds_to_programs(&op), bound, "{op:?}");
    }
}
