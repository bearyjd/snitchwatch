//! Shape policy for rule operators that reach root opensnitchd from a GUI
//! (`AddRule`/`UpdateRule` → [`crate::rule_wire::rule_from_wire`] →
//! `CHANGE_RULE`).
//!
//! opensnitchd v1.8.0 (`vendor/opensnitch/daemon/rule/operator.go`) picks
//! *what* to compare by the operand (`Match`: `dest.network`/`source.network`
//! pass a `net.IP`, the rest pass strings) but *how* by the type (`Compile`),
//! and the type's callback does hard type assertions with no `recover()`:
//!
//! - `simple` + `dest.network` reaches `simpleCmp`'s `v.(string)` and
//!   `network` + `dest.ip` reaches `cmpNetwork`'s `value.(net.IP)`: the
//!   daemon panics on the first matching connection. An `always` rule is
//!   saved and reloaded, so it crash-loops, and with `QueueBypass` the
//!   firewall fails open while the daemon is down.
//! - `Match` checks the operand `list` before the type, and `listMatch`
//!   starts from `true`: a list with no members, or a non-list type with
//!   the `list` operand, matches every connection. `Deserialize`
//!   (`rule.go`) copies list members only one level deep and drops members
//!   of a non-list type, so a nested list reaches the daemon empty.
//! - `lists.*` operands make the legacy daemon read every file under the
//!   directory named in `data`, as root. Snitchwatch creates those rules
//!   itself (blocklists), never from a GUI's rule; bazzite-tower's patched
//!   daemon refuses them from a UI too.
//!
//! [`validate_operator`] accepts only shapes that evaluate as written: a leaf
//! (`simple`, `regexp`, `network`) or one `list` of 1..=64 leaves. Every
//! shape the bridge itself builds (`translator::verdict`) passes. It runs
//! whatever the rule's `enabled` flag says: the daemon compiles only enabled
//! rules, so a disabled bad shape is stored unchecked until something
//! enables it.
//!
//! Errors are fixed text and never echo the operand or data: both are
//! client-supplied, and the errors reach logs.

use snitchwatch_proto::protocol::{Operator, Rule};

use crate::cache::rules::{MAX_OPERATOR_LIST_LEN, MAX_RULE_FIELD_BYTES};

/// Why a GUI may not edit a daemon rule whose operator fails
/// [`validate_operator`] (a `lists` blocklist rule, a network alias such as
/// `LAN`, or a shape the daemon can't evaluate). The rule is still listed;
/// the bridge refuses to send it back in a change, and the GUI also disables
/// Delete for it (a delete is by name only, so the bridge would forward one).
pub const SHAPE_READ_ONLY_REASON: &str = "Snitchwatch can't change or delete this rule because \
     of its conditions (a rule type, condition or network alias Snitchwatch won't send back to \
     the firewall service). The rule still applies.";

/// Operands whose value the daemon passes as a `net.IP`; only the `network`
/// type can compare one.
const NETWORK_OPERANDS: &[&str] = &["dest.network", "source.network"];

/// Operands whose value the daemon passes as a string (`operator.go`'s
/// `Operand` consts, minus `true`, `list`, the network ones and `lists.*`).
/// `process.env.<NAME>` is a prefix and is checked separately.
const STRING_OPERANDS: &[&str] = &[
    "process.id",
    "process.path",
    "process.parent.path",
    "process.command",
    "process.hash.md5",
    "process.hash.sha1",
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
];

const ENV_OPERAND_PREFIX: &str = "process.env.";

/// What `Match` hands a leaf's callback for its operand.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OperandKind {
    /// `true`: matches without calling the callback.
    True,
    /// A `net.IP`.
    Network,
    /// A string.
    Text,
}

/// Check that an operator from a GUI has a shape opensnitchd evaluates as
/// written: no panic, no silent match-all, no blocklist directory read.
pub fn validate_operator(op: &Operator) -> Result<(), String> {
    if op.r#type == "list" {
        validate_list(op)
    } else {
        validate_leaf(op)
    }
}

/// Why a daemon rule is listed read-only, or `None` when a GUI may edit it.
/// Uses the same checks as [`crate::rule_wire::rule_from_wire`], so a rule
/// the GUI may edit is one the bridge will send back.
pub fn read_only_reason(rule: &Rule) -> Option<&'static str> {
    if crate::rule_name::validate_rule_name(&rule.name).is_err() {
        return Some(crate::rule_wire::READ_ONLY_REASON);
    }
    match &rule.operator {
        Some(op) if validate_operator(op).is_ok() => None,
        _ => Some(SHAPE_READ_ONLY_REASON),
    }
}

fn validate_list(op: &Operator) -> Result<(), String> {
    // `Compile` sets a list's operand to `list`; `operator_from_wire` leaves
    // it empty.
    if !(op.operand.is_empty() || op.operand == "list") {
        return Err("a list operator's operand must be \"list\"".to_string());
    }
    if op.list.is_empty() {
        return Err("a list operator has no members; it would match every connection".to_string());
    }
    if op.list.len() > MAX_OPERATOR_LIST_LEN {
        return Err(format!(
            "a list operator has {} members; the limit is {MAX_OPERATOR_LIST_LEN}",
            op.list.len()
        ));
    }
    for (index, member) in op.list.iter().enumerate() {
        validate_leaf(member).map_err(|e| format!("list member {}: {e}", index + 1))?;
    }
    Ok(())
}

fn validate_leaf(op: &Operator) -> Result<(), String> {
    if !op.list.is_empty() {
        return Err("only a list operator may have members".to_string());
    }
    if op.operand.len() > MAX_RULE_FIELD_BYTES || op.data.len() > MAX_RULE_FIELD_BYTES {
        return Err(format!(
            "operator operand or data is over {MAX_RULE_FIELD_BYTES} bytes"
        ));
    }
    match op.r#type.as_str() {
        "simple" => match operand_kind(&op.operand)? {
            OperandKind::True | OperandKind::Text => Ok(()),
            OperandKind::Network => Err(NETWORK_OPERAND_NEEDS_NETWORK.to_string()),
        },
        "regexp" => match operand_kind(&op.operand)? {
            OperandKind::Text => validate_regexp(&op.data, op.sensitive),
            OperandKind::True => Err(TRUE_NEEDS_SIMPLE.to_string()),
            OperandKind::Network => Err(NETWORK_OPERAND_NEEDS_NETWORK.to_string()),
        },
        "network" => match operand_kind(&op.operand)? {
            OperandKind::Network => validate_cidr(&op.data),
            OperandKind::True | OperandKind::Text => {
                Err("the network type needs the dest.network or source.network operand".to_string())
            }
        },
        "list" => Err(
            "a list can't contain a list; the daemon drops the inner members and the empty \
             list matches every connection"
                .to_string(),
        ),
        "lists" => Err(LISTS_REFUSED.to_string()),
        _ => Err("operator type is not simple, regexp, network or list".to_string()),
    }
}

const NETWORK_OPERAND_NEEDS_NETWORK: &str =
    "the dest.network and source.network operands need the network type";
const TRUE_NEEDS_SIMPLE: &str = "the true operand needs the simple type";
const LISTS_REFUSED: &str =
    "blocklist (lists) rules are managed by Snitchwatch and can't be sent from a GUI";

fn operand_kind(operand: &str) -> Result<OperandKind, String> {
    if operand == "true" {
        return Ok(OperandKind::True);
    }
    if NETWORK_OPERANDS.contains(&operand) {
        return Ok(OperandKind::Network);
    }
    if STRING_OPERANDS.contains(&operand) {
        return Ok(OperandKind::Text);
    }
    if operand
        .strip_prefix(ENV_OPERAND_PREFIX)
        .is_some_and(is_env_var_name)
    {
        return Ok(OperandKind::Text);
    }
    if operand == "list" {
        return Err("the list operand needs the list type".to_string());
    }
    if operand.starts_with("lists.") {
        return Err(LISTS_REFUSED.to_string());
    }
    Err("unknown operator operand".to_string())
}

/// A portable environment variable name (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `Compile` lowercases an insensitive pattern before compiling it with Go's
/// RE2 `regexp`; Rust's `regex` is close enough to RE2 to catch a pattern
/// the daemon would refuse. The `regex::Error` is dropped: its text quotes
/// the pattern.
fn validate_regexp(data: &str, sensitive: bool) -> Result<(), String> {
    let pattern = if sensitive {
        data.to_string()
    } else {
        data.to_lowercase()
    };
    regex::Regex::new(&pattern)
        .map(|_| ())
        .map_err(|_| "operator data is not a valid regular expression".to_string())
}

/// `network` data must be a CIDR (`net.ParseCIDR`: address, `/`, decimal
/// prefix length). The daemon would also accept a network alias such as
/// `LAN`, but aliases come from its own `network_aliases.json`, which the
/// bridge can't see: an unknown alias fails `Compile` and the rule silently
/// never applies.
fn validate_cidr(data: &str) -> Result<(), String> {
    const NOT_A_CIDR: &str = "network operator data is not a CIDR such as 10.0.0.0/8";
    let (addr, prefix) = data.split_once('/').ok_or(NOT_A_CIDR)?;
    let max_prefix = match addr.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => 32,
        Ok(std::net::IpAddr::V6(_)) => 128,
        Err(_) => return Err(NOT_A_CIDR.to_string()),
    };
    let digits_only =
        !prefix.is_empty() && prefix.len() <= 3 && prefix.bytes().all(|b| b.is_ascii_digit());
    match prefix.parse::<u16>() {
        Ok(len) if digits_only && len <= max_prefix => Ok(()),
        _ => Err(NOT_A_CIDR.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::connections::Verdict;
    use crate::rule_wire::{rule_from_wire, rule_to_wire};
    use crate::translator::verdict::verdict_to_rule;
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
            "process.hash.md5",
            "process.hash.sha1",
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
                    let rule = verdict_to_rule(verdict, VerdictDuration::Always, scope, &conn, 0);
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
            op("regexp", "dest.host", ""),
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
}
