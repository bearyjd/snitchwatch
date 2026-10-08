//! Per-operand and comparison-semantics tests for `simulate`: the operand
//! table, networks and aliases, case folding, regexp lowercasing, hash
//! operands and operands that can't be simulated. Builders live in the parent
//! module.

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
fn regexp_lowercases_pattern_and_subject_unless_sensitive() {
    // Pattern uppercase, subject lowercase.
    assert!(matched(
        op("regexp", "process.path", "^/USR/BIN/CURL$"),
        &base()
    ));
    // Pattern lowercase, subject uppercase.
    let loud = base_with(|i| i.process_path = Some("/USR/BIN/CURL".to_string()));
    assert!(matched(
        op("regexp", "process.path", "^/usr/bin/curl$"),
        &loud
    ));
    // Sensitive: neither is touched.
    assert!(!matched(
        op_sensitive("regexp", "process.path", "^/USR/BIN/CURL$"),
        &base()
    ));
    assert!(!matched(
        op_sensitive("regexp", "process.path", "^/usr/bin/curl$"),
        &loud
    ));
    assert!(matched(
        op_sensitive("regexp", "process.path", "^/USR/BIN/CURL$"),
        &loud
    ));
}

#[test]
fn regexp_lowercasing_also_rewrites_class_escapes_like_the_daemon() {
    // `Operator.Compile` lowercases the whole pattern, so `\D` becomes `\d`.
    let digits = base_with(|i| i.process_path = Some("123".to_string()));
    let letters = base_with(|i| i.process_path = Some("abc".to_string()));
    let pattern = op("regexp", "process.path", r"^\D+$");
    assert!(matched(pattern.clone(), &digits));
    assert!(!matched(pattern, &letters));
    // Sensitive keeps `\D` as written.
    let pattern = op_sensitive("regexp", "process.path", r"^\D+$");
    assert!(!matched(pattern.clone(), &digits));
    assert!(matched(pattern, &letters));
}

#[test]
fn regexp_is_unanchored() {
    assert!(matched(op("regexp", "process.path", "curl"), &base()));
    assert!(!matched(op("regexp", "process.path", "^curl"), &base()));
}

#[test]
fn a_pattern_the_simulators_engine_cannot_compile_is_unsupported_never_a_miss() {
    // Every cached enabled rule already compiled under RE2 (the loader skips
    // rules that fail), so a failure here is a syntax difference between
    // engines, not a rule that cannot match. `\Q..\E` is RE2-only.
    for operator in [
        op_sensitive("regexp", "process.path", r"\Q/usr/bin\E/curl"),
        op("regexp", "process.path", "(unclosed"),
    ] {
        let result = run(operator.clone(), &base());
        assert_eq!(result.matched_rule, None, "{operator}");
        assert_eq!(result.unsupported_operands.len(), 1, "{operator}");
        let u = &result.unsupported_operands[0];
        assert_eq!(u.operand, "process.path");
        assert!(u.reason.contains("RE2"), "{}", u.reason);
    }
}

#[test]
fn a_big_bounded_repeat_still_compiles() {
    // Hostname-shaped patterns with `{1,253}` blow past the regex crate's
    // default 10 MiB program limit when `\w` is the Unicode class.
    let pattern = op("regexp", "dest.host", r"^[\w.-]{1,253}$");
    let host = |n: usize| base_with(|i| i.dest_host = "a".repeat(n));
    assert!(matched(pattern.clone(), &host(253)));
    assert!(!matched(pattern.clone(), &host(254)));
    assert!(!matched(pattern, &host(0)));
}

#[test]
fn a_unicode_class_repeat_needs_more_than_the_default_program_size() {
    // `\pL` stays a Unicode class under RE2; repeated up to RE2's limit of
    // 1000 it is far past the regex crate's 10 MiB default program size.
    let pattern = op_sensitive("regexp", "process.path", r"^\pL{1,1000}$");
    let at = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    let result = run(pattern.clone(), &at("éa"));
    assert_eq!(result.matched_rule.as_deref(), Some("r"), "{result:?}");
    assert!(!matched(pattern, &at("é1")));
}

#[test]
fn re2_reads_escaped_punctuation_and_an_unparsed_repeat_as_literals() {
    // RE2: a backslash before punctuation is that character (`\<` is `<`, not
    // a word-boundary assertion), and `{,2}` isn't a repeat, so it is text.
    let cases: &[(&str, &str, bool)] = &[
        ("a<b", r"a\<b", true),
        ("a>b", r"a\>b", true),
        ("ab", r"a\<b", false),
        ("x{,2}", r"^x{,2}$", true),
        ("xx", r"^x{,2}$", false),
        ("x", r"^x{,2}$", false),
        ("xxx", r"^x{2,3}$", true),
    ];
    for (subject, pattern, expected) in cases {
        let input = base_with(|i| i.process_path = Some(subject.to_string()));
        assert_eq!(
            matched(op_sensitive("regexp", "process.path", pattern), &input),
            *expected,
            "{pattern} vs {subject:?}"
        );
    }
}

#[test]
fn re2_perl_classes_are_ascii_only() {
    // RE2's `\w \d \s \b` are ASCII; Rust's are Unicode (and its `\s`
    // includes `\v` and NBSP). (subject, pattern, expected)
    let cases: &[(&str, &str, bool)] = &[
        ("/home/josé/bin", r"^/home/\w+/bin$", false),
        ("/home/jose/bin", r"^/home/\w+/bin$", true),
        ("\u{663}\u{664}", r"^\d+$", false), // Arabic-Indic digits
        ("34", r"^\d+$", true),
        ("\u{a0}", r"^\S+$", true), // NBSP is not RE2 space
        ("\u{a0}", r"^\s+$", false),
        ("\u{b}", r"^\s$", false), // \v is not RE2 space
        (" \t\n\r\u{c}", r"^\s+$", true),
        ("é", r"^\W$", true),
        ("é", r"^\D$", true),
        ("é", r"^[^\w]$", true),
        ("é", r"^[\W]$", true),
        ("é", r"^[\w]$", false),
        ("é", r"\bé\b", false), // ASCII word boundary: é is not a word char
        ("aé", r"a\b", true),
        ("a_1", r"^[\w]+$", true),
        ("a.b-c", r"^[\w.-]+$", true),
        ("a b", r"^[^\s]+$", false),
        ("é", r"^[^\w]+$", true),
    ];
    for (subject, pattern, expected) in cases {
        let input = base_with(|i| i.process_path = Some(subject.to_string()));
        // Sensitive, so the pattern isn't lowercased and `\W` stays `\W`.
        assert_eq!(
            matched(op_sensitive("regexp", "process.path", pattern), &input),
            *expected,
            "{pattern} vs {subject:?}"
        );
    }
}

#[test]
fn a_bracket_inside_a_class_is_a_literal_like_in_go() {
    // Go: `[[a]]` is the class {'[', 'a'} followed by a literal `]`; the
    // regex crate would read a nested class.
    let at = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    let pattern = op_sensitive("regexp", "process.path", r"^[[a]]$");
    assert!(matched(pattern.clone(), &at("[]")));
    assert!(matched(pattern.clone(), &at("a]")));
    assert!(!matched(pattern, &at("a")));
    // A POSIX class still works, and a leading `]` is a literal.
    assert!(matched(
        op_sensitive("regexp", "process.path", r"^[[:alpha:]]+$"),
        &at("abc")
    ));
    assert!(matched(
        op_sensitive("regexp", "process.path", r"^[]a]+$"),
        &at("]a]")
    ));
}

#[test]
fn escaped_backslashes_are_not_read_as_classes() {
    // `\\w` is a literal backslash then `w`.
    let at = |p: &str| base_with(|i| i.process_path = Some(p.to_string()));
    let pattern = op_sensitive("regexp", "process.path", r"^\\w$");
    assert!(matched(pattern.clone(), &at("\\w")));
    assert!(!matched(pattern, &at("a")));
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

// ---- hash operands ---------------------------------------------------------

fn hash_rule() -> Value {
    simple("process.hash.md5", "deadbeef")
}

fn with_checksums(enabled: Option<bool>, sums: Option<&[(&str, &str)]>) -> SimulationInput {
    base_with(|i| {
        i.checksums_enabled = enabled;
        i.checksums = sums.map(|s| {
            s.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        });
    })
}

#[test]
fn hash_matches_every_program_while_checksums_are_off() {
    let result = run(
        hash_rule(),
        &with_checksums(Some(false), Some(&[("md5", "cafe")])),
    );
    assert_eq!(result.matched_rule.as_deref(), Some("r"));
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("match every program while checksums are off")),
        "{:?}",
        result.warnings
    );
}

#[test]
fn hash_with_the_checksum_setting_unknown_is_decided_only_when_on_and_off_agree() {
    // Off always matches; so the rule is decided only if the checksums-on
    // answer is a match too.
    let hit = run(
        hash_rule(),
        &with_checksums(None, Some(&[("md5", "deadbeef")])),
    );
    assert_eq!(hit.matched_rule.as_deref(), Some("r"));
    assert!(hit.unevaluated.is_empty());

    let none_recorded = run(hash_rule(), &with_checksums(None, Some(&[])));
    assert_eq!(none_recorded.matched_rule.as_deref(), Some("r"));
    assert!(
        none_recorded
            .warnings
            .iter()
            .any(|w| w.contains("no recorded checksum")),
        "{:?}",
        none_recorded.warnings
    );

    // On: a mismatch. Off: a match. Not decidable.
    let miss = run(
        hash_rule(),
        &with_checksums(None, Some(&[("md5", "cafebabe")])),
    );
    assert_eq!(miss.matched_rule, None);
    assert_eq!(miss.unevaluated.len(), 1);
    assert_eq!(miss.unevaluated[0].operand, "process.hash.md5");
    assert!(miss.unevaluated[0].missing.contains("checksums are on"));
}

#[test]
fn hash_with_nothing_known_about_checksums_is_not_a_default_match() {
    // The form's default: checksum setting and checksum both unknown.
    let result = run(hash_rule(), &with_checksums(None, None));
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    assert!(result.unevaluated[0].missing.contains("checksums are on"));
}

#[test]
fn a_hash_deny_does_not_decide_while_the_checksum_setting_is_unknown() {
    let rules = vec![
        deny("100-deny-hash", hash_rule()),
        allow("200-allow", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &with_checksums(None, None));
    assert_eq!(result.matched_rule.as_deref(), Some("200-allow"));
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].rule, "100-deny-hash");
}

#[test]
fn hash_with_checksums_on_compares_the_programs_checksum() {
    let hit = run(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("md5", "deadbeef")])),
    );
    assert_eq!(hit.matched_rule.as_deref(), Some("r"));
    assert!(hit.warnings.is_empty(), "{:?}", hit.warnings);

    let miss = run(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("md5", "cafebabe")])),
    );
    assert_eq!(miss.matched_rule, None);
    assert!(miss.unevaluated.is_empty());
}

#[test]
fn hash_with_checksums_on_but_none_recorded_still_matches_with_a_warning() {
    // `ret` starts true and is only overwritten while iterating the process's
    // checksums, so a process with none matches every hash rule.
    let result = run(hash_rule(), &with_checksums(Some(true), Some(&[])));
    assert_eq!(result.matched_rule.as_deref(), Some("r"));
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("no recorded checksum")),
        "{:?}",
        result.warnings
    );
}

#[test]
fn hash_with_checksums_on_and_the_programs_checksum_unknown_is_unevaluated() {
    let result = run(hash_rule(), &with_checksums(Some(true), None));
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].rule, "r");
    assert_eq!(result.unevaluated[0].operand, "process.hash.md5");
}

#[test]
fn hash_compare_is_exact_and_tries_every_recorded_algorithm() {
    // `hashCmp` is a plain `==`: no case folding, whatever `sensitive` says.
    let upper = simple("process.hash.md5", "DEADBEEF");
    assert!(!matched(
        upper,
        &with_checksums(Some(true), Some(&[("md5", "deadbeef")]))
    ));
    // `Match` iterates every checksum the process has, whatever the operand's
    // algorithm: an md5 rule matches an equal sha1 value.
    assert!(matched(
        simple("process.hash.sha1", "deadbeef"),
        &with_checksums(Some(true), Some(&[("md5", "deadbeef")]))
    ));
    // An empty recorded checksum is a fake match ("avoid displaying a pop-up").
    assert!(matched(
        hash_rule(),
        &with_checksums(Some(true), Some(&[("md5", "")]))
    ));
    // A regexp-typed hash rule goes through `reCmp`.
    let sums = with_checksums(Some(true), Some(&[("md5", "deadbeef")]));
    assert!(matched(op("regexp", "process.hash.md5", "^dead"), &sums));
    assert!(!matched(op("regexp", "process.hash.md5", "^beef"), &sums));
}

#[test]
fn a_hash_warning_is_dropped_when_the_rule_cannot_match_anyway() {
    let operator = list_op(vec![hash_rule(), simple("process.path", "/usr/bin/wget")]);
    let result = run(operator, &with_checksums(Some(false), None));
    assert_eq!(result.matched_rule, None);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

#[test]
fn the_hash_warning_comes_from_the_deciding_rule_only() {
    // An earlier hash allow that is overwritten by a later allow is not the
    // reason for the verdict, so its warning is not shown.
    let rules = vec![
        allow("100-hash", hash_rule()),
        allow("200-host", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &with_checksums(Some(false), None));
    assert_eq!(result.matched_rule.as_deref(), Some("200-host"));
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
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
