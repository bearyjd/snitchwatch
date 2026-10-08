//! Per-operand tests for `simulate`: the operand table, networks and aliases,
//! case folding, lists and operands that can't be simulated. Builders live in
//! the parent module; regexps and hashes have their own files.

use super::*;

// ---- operand table: one positive and one negative case per operand ---------

struct Case {
    label: &'static str,
    operator: Value,
    input: SimulationInput,
    expect_match: bool,
}

fn case(label: &'static str, operator: Value, input: SimulationInput, expect_match: bool) -> Case {
    Case {
        label,
        operator,
        input,
        expect_match,
    }
}

#[test]
fn every_operand_has_a_positive_and_a_negative_case() {
    let ancestors = paths(&["/usr/bin/bash", "/usr/lib/systemd/systemd"]);
    let cases = vec![
        case(
            "process.path +",
            simple("process.path", "/usr/bin/curl"),
            base(),
            true,
        ),
        case(
            "process.path -",
            simple("process.path", "/usr/bin/wget"),
            base(),
            false,
        ),
        // `process.parent.path` walks every ancestor, not just the parent.
        case(
            "parent.path + (grandparent)",
            simple("process.parent.path", "/usr/lib/systemd/systemd"),
            base_with(|i| i.parent_paths = ancestors.clone()),
            true,
        ),
        case(
            "parent.path + (parent)",
            simple("process.parent.path", "/usr/bin/bash"),
            base_with(|i| i.parent_paths = ancestors.clone()),
            true,
        ),
        case(
            "parent.path -",
            simple("process.parent.path", "/usr/bin/zsh"),
            base_with(|i| i.parent_paths = ancestors.clone()),
            false,
        ),
        case(
            "parent.path - (no ancestors at all)",
            simple("process.parent.path", "/usr/bin/bash"),
            base_with(|i| i.parent_paths = paths(&[])),
            false,
        ),
        case(
            "parent.path regexp +",
            op("regexp", "process.parent.path", "systemd$"),
            base_with(|i| i.parent_paths = ancestors.clone()),
            true,
        ),
        // `process.command` is the args joined with single spaces.
        case(
            "command +",
            simple("process.command", "/usr/bin/curl -s https://example.com"),
            base_with(|i| i.command = Some("/usr/bin/curl -s https://example.com".to_string())),
            true,
        ),
        case(
            "command - (only the whole joined line is compared)",
            simple("process.command", "/usr/bin/curl"),
            base_with(|i| i.command = Some("/usr/bin/curl -s https://example.com".to_string())),
            false,
        ),
        case(
            "command regexp +",
            op("regexp", "process.command", r"https://example\.com$"),
            base_with(|i| i.command = Some("/usr/bin/curl -s https://example.com".to_string())),
            true,
        ),
        case(
            "process.id +",
            simple("process.id", "1234"),
            base_with(|i| i.pid = Some(1234)),
            true,
        ),
        case(
            "process.id -",
            simple("process.id", "12345"),
            base_with(|i| i.pid = Some(1234)),
            false,
        ),
        case(
            "user.id +",
            simple("user.id", "1000"),
            base_with(|i| i.uid = Some(1000)),
            true,
        ),
        case(
            "user.id + root",
            simple("user.id", "0"),
            base_with(|i| i.uid = Some(0)),
            true,
        ),
        case(
            "user.id -",
            simple("user.id", "0"),
            base_with(|i| i.uid = Some(1000)),
            false,
        ),
        case(
            "env +",
            simple("process.env.HOME", "/home/u"),
            base_with(|i| i.env = env_of(&[("HOME", "/home/u")])),
            true,
        ),
        case(
            "env -",
            simple("process.env.HOME", "/root"),
            base_with(|i| i.env = env_of(&[("HOME", "/home/u")])),
            false,
        ),
        // An unset variable compares as "" when the environment is known.
        case(
            "env unset compares as empty +",
            simple("process.env.LANG", ""),
            base_with(|i| i.env = env_of(&[("HOME", "/home/u")])),
            true,
        ),
        case(
            "env unset compares as empty -",
            simple("process.env.LANG", "C"),
            base_with(|i| i.env = env_of(&[("HOME", "/home/u")])),
            false,
        ),
        // The daemon trims "\r\n\t " from the variable name in the operand.
        case(
            "env name is trimmed",
            simple("process.env.HOME\t", "/home/u"),
            base_with(|i| i.env = env_of(&[("HOME", "/home/u")])),
            true,
        ),
        case(
            "source.ip +",
            simple("source.ip", "192.168.1.5"),
            base_with(|i| i.src_ip = Some("192.168.1.5".to_string())),
            true,
        ),
        case(
            "source.ip -",
            simple("source.ip", "192.168.1.6"),
            base_with(|i| i.src_ip = Some("192.168.1.5".to_string())),
            false,
        ),
        // The daemon compares Go's net.IP.String(): lowercase, compressed.
        case(
            "source.ip canonical IPv6",
            simple("source.ip", "2001:db8::1"),
            base_with(|i| i.src_ip = Some("2001:DB8:0:0:0:0:0:1".to_string())),
            true,
        ),
        case(
            "source.port +",
            simple("source.port", "51000"),
            base_with(|i| i.src_port = Some(51000)),
            true,
        ),
        case(
            "source.port -",
            simple("source.port", "443"),
            base_with(|i| i.src_port = Some(51000)),
            false,
        ),
        case(
            "dest.ip +",
            simple("dest.ip", "93.184.216.34"),
            base_with(|i| i.dest_ip = Some("93.184.216.34".to_string())),
            true,
        ),
        case(
            "dest.ip -",
            simple("dest.ip", "93.184.216.35"),
            base_with(|i| i.dest_ip = Some("93.184.216.34".to_string())),
            false,
        ),
        // A v4-mapped address prints as a dotted quad.
        case(
            "dest.ip v4-mapped prints as IPv4",
            simple("dest.ip", "93.184.216.34"),
            base_with(|i| i.dest_ip = Some("::ffff:93.184.216.34".to_string())),
            true,
        ),
        case(
            "dest.host +",
            simple("dest.host", "example.com"),
            base(),
            true,
        ),
        case(
            "dest.host -",
            simple("dest.host", "other.example"),
            base(),
            false,
        ),
        // A bare IP has DstHost "".
        case(
            "dest.host empty (bare IP) +",
            simple("dest.host", ""),
            base_with(|i| i.dest_host = String::new()),
            true,
        ),
        case(
            "dest.host empty (bare IP) -",
            simple("dest.host", "example.com"),
            base_with(|i| i.dest_host = String::new()),
            false,
        ),
        case("dest.port +", simple("dest.port", "443"), base(), true),
        case("dest.port -", simple("dest.port", "80"), base(), false),
        case("protocol +", simple("protocol", "tcp"), base(), true),
        case(
            "protocol - (tcp6 is not tcp)",
            simple("protocol", "tcp"),
            base_with(|i| i.protocol = "tcp6".to_string()),
            false,
        ),
        case(
            "protocol folds case",
            simple("protocol", "TCP"),
            base(),
            true,
        ),
        case(
            "iface.in +",
            simple("iface.in", "eth0"),
            base_with(|i| i.iface_in = Some("eth0".to_string())),
            true,
        ),
        case(
            "iface.in -",
            simple("iface.in", "wlan0"),
            base_with(|i| i.iface_in = Some("eth0".to_string())),
            false,
        ),
        case(
            "iface.out +",
            simple("iface.out", "wlan0"),
            base_with(|i| i.iface_out = Some("wlan0".to_string())),
            true,
        ),
        case(
            "iface.out -",
            simple("iface.out", "eth0"),
            base_with(|i| i.iface_out = Some("wlan0".to_string())),
            false,
        ),
        case("true +", simple("true", ""), base(), true),
        // An empty list starts from `true` and has nothing to falsify it.
        case("empty list matches", list_op(vec![]), base(), true),
        // Operands outside `Match`'s chain fall through to `return false`.
        case(
            "unknown operand never matches",
            simple("quota.sent.over", "1mb"),
            base(),
            false,
        ),
    ];
    for c in cases {
        assert_eq!(
            matched(c.operator.clone(), &c.input),
            c.expect_match,
            "{}: operator {} vs input {:?}",
            c.label,
            c.operator,
            c.input
        );
    }
}

#[test]
fn network_operands_use_cidr_math_and_the_daemons_alias_table() {
    // (operand, data, ip, expected)
    let cases: &[(&str, &str, &str, bool)] = &[
        ("dest.network", "10.0.0.0/8", "10.1.2.3", true),
        ("dest.network", "10.0.0.0/8", "11.0.0.1", false),
        // ParseCIDR masks host bits.
        ("dest.network", "10.1.2.3/8", "10.9.9.9", true),
        ("dest.network", "192.168.1.1/32", "192.168.1.1", true),
        ("dest.network", "192.168.1.1/32", "192.168.1.2", false),
        ("dest.network", "0.0.0.0/0", "8.8.8.8", true),
        // Different families never contain each other.
        ("dest.network", "0.0.0.0/0", "2001:db8::1", false),
        ("dest.network", "2001:db8::/32", "10.0.0.1", false),
        ("dest.network", "2001:db8::/32", "2001:db8::1", true),
        ("dest.network", "2001:db8::/32", "2001:db9::1", false),
        // An IPv4 network contains the v4-mapped form of its addresses.
        ("dest.network", "10.0.0.0/8", "::ffff:10.1.2.3", true),
        // Aliases come from `network_aliases.json` (exact, case-sensitive).
        ("dest.network", "LAN", "192.168.1.1", true),
        ("dest.network", "LAN", "172.20.0.1", true),
        ("dest.network", "LAN", "127.0.0.1", true),
        ("dest.network", "LAN", "fd00::1", true),
        ("dest.network", "LAN", "8.8.8.8", false),
        ("dest.network", "LAN", "fe80::1", false),
        // The shipped file lists "::1" without a prefix length; the daemon's
        // `LoadAliases` skips entries `net.ParseCIDR` rejects, so `LAN` does
        // not contain the IPv6 loopback address.
        ("dest.network", "LAN", "::1", false),
        ("dest.network", "MULTICAST", "224.0.0.251", true),
        ("dest.network", "MULTICAST", "ff02::fb", true),
        ("dest.network", "MULTICAST", "10.0.0.1", false),
        // A v4-mapped CIDR is read as the IPv4 network it names
        // (`IPNet.Contains` takes `To4()` of both and the low 32 mask bits)...
        ("dest.network", "::ffff:0:0/96", "8.8.8.8", true),
        ("dest.network", "::ffff:10.0.0.0/104", "10.1.2.3", true),
        ("dest.network", "::ffff:10.0.0.0/104", "11.0.0.1", false),
        // ...unless the prefix is short enough that masking clears the
        // `ffff` marker, which leaves a plain IPv6 network.
        ("dest.network", "::ffff:0:0/95", "8.8.8.8", false),
        ("source.network", "10.0.0.0/8", "10.1.2.3", true),
        ("source.network", "10.0.0.0/8", "11.1.2.3", false),
        ("source.network", "LAN", "192.168.0.7", true),
    ];
    for (operand, data, ip, expected) in cases {
        let input = base_with(|i| {
            i.dest_ip = Some(ip.to_string());
            i.src_ip = Some(ip.to_string());
        });
        assert_eq!(
            matched(op("network", operand, data), &input),
            *expected,
            "{operand} {data} vs {ip}"
        );
    }
}

#[test]
fn a_network_the_simulator_cannot_resolve_is_unsupported_never_a_match() {
    let input = base_with(|i| i.dest_ip = Some("10.0.0.1".to_string()));
    for data in ["lan", "HOMENET", "not-a-cidr", ""] {
        let result = run(op("network", "dest.network", data), &input);
        assert!(result.matched_rule.is_none(), "{data:?} matched");
        assert_eq!(
            result.unsupported_operands.len(),
            1,
            "{data:?}: {:?}",
            result.unsupported_operands
        );
        assert_eq!(result.unsupported_operands[0].operand, "dest.network");
    }
}

#[test]
fn operator_type_and_operand_pairings_that_crash_the_daemon_are_unsupported() {
    // `simpleCmp` does `v.(string)` on a net.IP; `cmpNetwork` does
    // `value.(net.IP)` on a string — both panic in the daemon.
    let input = base_with(|i| i.dest_ip = Some("10.0.0.1".to_string()));
    for operator in [
        simple("dest.network", "10.0.0.0/8"),
        op("network", "dest.ip", "10.0.0.0/8"),
        op("network", "dest.host", "10.0.0.0/8"),
    ] {
        let result = run(operator.clone(), &input);
        assert!(result.matched_rule.is_none(), "{operator} matched");
        assert_eq!(result.unsupported_operands.len(), 1, "{operator}");
    }
    // `reCmp` refuses a non-string subject and returns false.
    let result = run(op("regexp", "dest.network", ".*"), &input);
    assert!(result.matched_rule.is_none());
    assert!(result.unsupported_operands.is_empty());
}

// ---- comparison semantics --------------------------------------------------

#[test]
fn simple_compare_folds_case_like_go_equal_fold() {
    // ASCII, and non-ASCII (`EqualFold` is Unicode, ASCII-only folding isn't).
    let path = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    assert!(matched(simple("process.path", "/USR/BIN/CURL"), &base()));
    assert!(matched(
        simple("process.path", "/opt/ünï"),
        &path("/opt/Ünï")
    ));
    assert!(matched(
        simple("process.path", "/opt/k"),
        &path("/opt/\u{212A}")
    ));
    // ς, σ and Σ are one case-folding orbit; ı is not folded with i.
    assert!(matched(simple("process.path", "σ"), &path("ς")));
    assert!(!matched(simple("process.path", "i"), &path("ı")));
    assert!(!matched(simple("process.path", "ss"), &path("ß")));
}

#[test]
fn sensitive_simple_compare_is_exact() {
    assert!(!matched(
        op_sensitive("simple", "process.path", "/USR/BIN/CURL"),
        &base()
    ));
    assert!(matched(
        op_sensitive("simple", "process.path", "/usr/bin/curl"),
        &base()
    ));
    assert!(!matched(
        op_sensitive("simple", "dest.host", "Example.com"),
        &base()
    ));
}

#[test]
fn list_children_are_anded() {
    let flat = list_op(vec![
        simple("protocol", "tcp"),
        simple("dest.port", "443"),
        simple("dest.host", "example.com"),
    ]);
    assert!(matched(flat.clone(), &base()));
    let wrong_port = base_with(|i| i.dest_port = 80);
    assert!(!matched(flat, &wrong_port));
    // A list type wins over the operand string (`Compile` sets operand=list).
    let weird =
        json!({"type": "list", "operand": "true", "data": "", "list": [simple("protocol", "udp")]});
    assert!(!matched(weird, &base()));
}

#[test]
fn a_list_inside_a_list_is_unsupported_not_anded() {
    // opensnitchd compiles `Operator.List[i]` one level deep; a nested list's
    // members are never compiled (nil callback) and are dropped entirely for
    // rules sent over gRPC, so it panics or matches everything — never ANDs.
    let nested = list_op(vec![
        simple("protocol", "tcp"),
        list_op(vec![simple("dest.port", "443")]),
    ]);
    let result = run(nested, &base());
    assert_eq!(result.matched_rule, None);
    assert_eq!(
        result.unsupported_operands.len(),
        1,
        "{:?}",
        result.unsupported_operands
    );
    assert_eq!(result.unsupported_operands[0].operand, "list");
    assert!(result.unsupported_operands[0]
        .reason
        .contains("list inside a list"));

    // A member before it that fails decides the list first (short-circuit),
    // so the nested list is never reached.
    let nested = list_op(vec![
        simple("protocol", "udp"),
        list_op(vec![simple("dest.port", "443")]),
    ]);
    let result = run(nested, &base());
    assert_eq!(result.matched_rule, None);
    assert!(result.unsupported_operands.is_empty());
}

// ---- unsupported operands --------------------------------------------------

#[test]
fn user_name_is_unsupported_with_its_reason() {
    let result = run(
        simple("user.name", "alice"),
        &base_with(|i| i.uid = Some(1000)),
    );
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unsupported_operands.len(), 1);
    let u = &result.unsupported_operands[0];
    assert_eq!(u.operand, "user.name");
    assert!(u.reason.contains("user id"), "{}", u.reason);
}

#[test]
fn lists_operands_are_unsupported_with_their_reason() {
    for operand in [
        "lists.domains",
        "lists.domains_regexp",
        "lists.ips",
        "lists.nets",
        "lists.hash.md5",
    ] {
        let result = run(op("lists", operand, "/etc/opensnitchd/lists"), &base());
        assert_eq!(result.matched_rule, None, "{operand}");
        assert_eq!(result.unsupported_operands.len(), 1, "{operand}");
        let u = &result.unsupported_operands[0];
        assert_eq!(u.operand, operand);
        assert!(u.reason.contains("list contents"), "{}", u.reason);
    }
}

#[test]
fn an_operand_match_has_no_branch_for_is_false_not_unsupported() {
    // `Match` falls through to `return false`: for opensnitchd 1.8.0 such a
    // rule can never match, so there is nothing "unsupported" to report.
    for operand in ["quota.sent.over", "dest.hostname", "process_path", ""] {
        for kind in ["simple", "regexp"] {
            let result = run(op(kind, operand, ".*"), &base());
            assert_eq!(result.matched_rule, None, "{kind} {operand:?}");
            assert!(result.unsupported_operands.is_empty(), "{kind} {operand:?}");
            assert!(result.unevaluated.is_empty(), "{kind} {operand:?}");
        }
    }
}

#[test]
fn a_blank_process_path_is_unknown_not_an_empty_path() {
    // Compared as "", every `process.path` rule would be a quiet non-match
    // and the verdict could fall through to a host-wide rule.
    let unknown = base_with(|i| i.process_path = None);
    let result = run(simple("process.path", "/usr/bin/curl"), &unknown);
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].operand, "process.path");
    assert!(result.unevaluated[0].missing.contains("process path"));
    // A rule that doesn't look at the path is still decided.
    assert!(matched(simple("dest.host", "example.com"), &unknown));
}
