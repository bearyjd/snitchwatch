//! PR 4a: redaction-equivalence, collision and bound tests for
//! `rule_identity`. No database.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use snitchwatch_proto::protocol::{Operator, Rule};

use super::caps::{RULE_CLASS, RULE_HASH, SNAPSHOT};
use super::rule_identity::*;

fn op(kind: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: kind.into(),
        operand: operand.into(),
        data: data.into(),
        sensitive: false,
        list: vec![],
    }
}

fn rule(name: &str, operator: Option<Operator>) -> Rule {
    Rule {
        name: name.into(),
        action: "allow".into(),
        duration: "always".into(),
        enabled: true,
        operator,
        ..Rule::default()
    }
}

fn group(members: Vec<Operator>) -> Operator {
    Operator {
        list: members,
        ..op("list", "list", "[]")
    }
}

fn id(r: &Rule) -> RuleIdentity {
    identify(r).expect("identity")
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// A rule with every withheld kind, in a nested list, with `v` in each
/// withheld slot (the description and creation time included).
fn withheld_variant(v: &str) -> Rule {
    let json = format!("[{{\"operand\":\"process.command\",\"data\":\"{v}\"}}]");
    let nested = group(vec![
        op("simple", "process.command", v),
        op("regexp", "process.env.HOME", v),
        op("simple", "process.env.NAME", v),
        op("simple", "x-unknown-kind", v),
        op("simple", "process.hash.md5", v),
        op("simple", "user.name", v),
    ]);
    let mut top = group(vec![op("simple", "dest.host", "example.org"), nested]);
    top.data = json;
    Rule {
        description: v.into(),
        created: v.len() as i64,
        ..rule("r", Some(top))
    }
}

#[test]
fn varying_only_withheld_values_leaves_bytes_and_hash_identical() {
    let a = id(&withheld_variant("canary-one"));
    let b = id(&withheld_variant("a-very-different-and-longer-secret"));
    assert_eq!(a.canonical, b.canonical);
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.qualifiers, b.qualifiers);
}

#[test]
fn withheld_values_never_appear_in_the_canonical_bytes() {
    let ident = id(&withheld_variant("CANARY-SECRET"));
    assert!(!contains(&ident.canonical, "CANARY"));
    assert!(contains(&ident.canonical, "example.org"));
}

#[test]
fn changing_only_sensitive_changes_the_hash_even_when_nested() {
    let leaf = |s| Operator {
        sensitive: s,
        ..op("simple", "process.path", "/usr/bin/x")
    };
    let top = |s| Operator {
        sensitive: s,
        ..group(vec![op("simple", "dest.port", "443")])
    };
    assert_ne!(
        id(&rule("r", Some(leaf(false)))).hash,
        id(&rule("r", Some(leaf(true)))).hash
    );
    assert_ne!(
        id(&rule("r", Some(top(false)))).hash,
        id(&rule("r", Some(top(true)))).hash
    );
    let nested = |s| group(vec![op("simple", "dest.port", "1"), leaf(s)]);
    assert_ne!(
        id(&rule("r", Some(nested(false)))).hash,
        id(&rule("r", Some(nested(true)))).hash
    );
    // A withheld operator's sensitive flag is structure, not a value.
    let cmd = |s| Operator {
        sensitive: s,
        ..op("simple", "process.command", "x")
    };
    assert_ne!(
        id(&rule("r", Some(cmd(false)))).hash,
        id(&rule("r", Some(cmd(true)))).hash
    );
}

#[test]
fn qualifier_flags() {
    let plain = id(&rule("r", Some(op("simple", "dest.host", "a.b"))));
    assert!(!plain.qualifiers.list_contents_not_recorded);
    assert!(!plain.qualifiers.partly_not_recorded());

    let lists = id(&rule("r", Some(op("lists", "lists.domains", "/d"))));
    assert!(lists.qualifiers.list_contents_not_recorded);
    assert!(contains(&lists.canonical, "/d"), "directory is kept");
    assert!(!lists.qualifiers.partly_not_recorded());

    let partly = id(&withheld_variant("x"));
    assert!(partly.qualifiers.partly_not_recorded());
    assert_eq!(partly.qualifiers.withheld_conditions, 6);
    assert!(!partly.qualifiers.list_contents_not_recorded);
}

#[test]
fn path_and_host_values_do_appear_the_stated_limit() {
    let r = rule(
        "r",
        Some(group(vec![
            op("simple", "process.path", "/home/u/PATH-CANARY"),
            op("simple", "dest.host", "HOST-CANARY.example"),
        ])),
    );
    let ident = id(&r);
    assert!(contains(&ident.canonical, "PATH-CANARY"));
    assert!(contains(&ident.canonical, "HOST-CANARY"));
}

#[test]
fn hash_is_sixteen_bytes_of_sha256_over_the_canonical_bytes() {
    let ident = id(&withheld_variant("x"));
    assert_eq!(ident.hash.len(), 16);
    assert_eq!(RULE_HASH, 16);
    assert_eq!(ident.hash[..], Sha256::digest(&ident.canonical)[..16]);
    assert!(ident.hash.iter().any(|b| *b != 0));
}

#[test]
fn identity_is_deterministic_and_has_no_timestamp() {
    let mut a = withheld_variant("x");
    let ha = id(&a).hash;
    a.created = 12345;
    assert_eq!(id(&a).hash, ha);
}

/// Each change below is a distinct rule: every one must change the hash.
#[test]
fn every_visible_field_changes_the_hash() {
    let base = || rule("r", Some(op("simple", "dest.host", "a.b")));
    let mut variants: Vec<Rule> = Vec::new();
    let mut with = |f: &dyn Fn(&mut Rule)| {
        let mut r = base();
        f(&mut r);
        variants.push(r);
    };
    with(&|r| r.name = "s".into());
    with(&|r| r.action = "deny".into());
    with(&|r| r.duration = "once".into());
    with(&|r| r.enabled = false);
    with(&|r| r.precedence = true);
    with(&|r| r.nolog = true);
    with(&|r| r.operator = None);
    with(&|r| r.operator = Some(op("regexp", "dest.host", "a.b")));
    with(&|r| r.operator = Some(op("simple", "dest.ip", "a.b")));
    with(&|r| r.operator = Some(op("simple", "dest.host", "a.c")));
    with(&|r| r.operator = Some(op("simple", "process.command", "a.b")));
    let mut seen = HashMap::new();
    seen.insert(id(&base()).hash, "base");
    for (i, v) in variants.iter().enumerate() {
        assert!(seen.insert(id(v).hash, "variant").is_none(), "variant {i}");
    }
}

#[test]
fn field_boundaries_and_structure_cannot_collide() {
    let at = |name: &str, data: &str| rule(name, Some(op("simple", "dest.host", data)));
    assert_ne!(id(&at("ab", "c")).hash, id(&at("a", "bc")).hash);
    assert_ne!(id(&at("", "x")).hash, id(&at("x", "")).hash);
    // A length-prefix lookalike inside a value.
    assert_ne!(
        id(&at("a", "\0\0\0\x01b")).hash,
        id(&at("a\0\0\0\x01", "b")).hash
    );
    let a = op("simple", "dest.port", "1");
    let b = op("simple", "dest.port", "2");
    let nest = |o: Operator, m: Vec<Operator>| Operator { list: m, ..o };
    let flat = group(vec![a.clone(), b.clone()]);
    let deep = group(vec![nest(a.clone(), vec![b.clone()])]);
    let other = group(vec![group(vec![a.clone()]), b.clone()]);
    let hashes = [flat, deep, other].map(|o| id(&rule("r", Some(o))).hash);
    assert_ne!(hashes[0], hashes[1]);
    assert_ne!(hashes[0], hashes[2]);
    assert_ne!(hashes[1], hashes[2]);
    // Member order is part of the form.
    let swapped = group(vec![b, a]);
    assert_ne!(id(&rule("r", Some(swapped))).hash, hashes[0]);
    // Kept-empty versus withheld.
    assert_ne!(
        id(&rule("r", Some(op("simple", "dest.host", "")))).hash,
        id(&rule("r", Some(op("simple", "process.command", "")))).hash
    );
}

/// A value that spells out the encoding of a child operator must not equal
/// the real child (only length prefixes tell them apart).
#[test]
fn a_value_cannot_forge_a_nested_operator() {
    let child = op("simple", "dest.port", "1");
    let real = Operator {
        list: vec![child],
        ..op("simple", "dest.host", "x")
    };
    let forged_data = "x\0\0\0\x01simpledest.port\0\x011";
    let forged = op("simple", "dest.host", forged_data);
    assert_ne!(
        id(&rule("r", Some(real))).hash,
        id(&rule("r", Some(forged))).hash
    );
}

// ---- allowlist ------------------------------------------------------

#[test]
fn unknown_types_and_actions_are_not_recorded_but_kinds_are_kept() {
    let mk = |kind: &str, action: &str, dur: &str| Rule {
        action: action.into(),
        duration: dur.into(),
        ..rule("r", Some(op(kind, "weird.kind", "d")))
    };
    let a = id(&mk("CANARY-T", "CANARY-A", "CANARY!"));
    let b = id(&mk("OTHER-T", "OTHER-A", "other!"));
    assert_eq!(a.canonical, b.canonical);
    assert!(!contains(&a.canonical, "CANARY"));
    assert!(contains(&a.canonical, "weird.kind"));
    let deny = id(&mk("simple", "deny", "once")).hash;
    assert_ne!(id(&mk("simple", "allow", "once")).hash, deny);
}

#[test]
fn env_var_name_and_unknown_kind_are_part_of_the_kind() {
    let h = |operand: &str, data: &str| id(&rule("r", Some(op("simple", operand, data)))).hash;
    assert_ne!(h("process.env.A", "v"), h("process.env.B", "v"));
    assert_ne!(h("future.kind.a", "v"), h("future.kind.b", "v"));
    assert_eq!(h("process.env.A", "v"), h("process.env.A", "other"));
    assert_eq!(h("future.kind.a", "v"), h("future.kind.a", "other"));
    let cmd = id(&rule("r", Some(op("simple", "process.env.A", "SECRET"))));
    assert!(contains(&cmd.canonical, "process.env.A"));
    assert!(!contains(&cmd.canonical, "SECRET"));
    assert_eq!(cmd.qualifiers.withheld_conditions, 1);
}

#[test]
fn list_json_is_withheld() {
    let j1 = Operator {
        data: "[\"CANARY\"]".into(),
        ..group(vec![])
    };
    assert!(!contains(&id(&rule("r", Some(j1))).canonical, "CANARY"));
}

#[test]
fn a_list_type_operator_never_keeps_data_whatever_its_operand() {
    for operand in ["dest.host", "process.path", "process.command", ""] {
        let o = op(
            "list",
            operand,
            "[{\"operand\":\"process.command\",\"data\":\"CANARY\"}]",
        );
        let ident = id(&rule("r", Some(o)));
        assert!(!contains(&ident.canonical, "CANARY"), "{operand}");
        assert_eq!(ident.qualifiers.withheld_conditions, 0, "{operand}");
    }
}

#[test]
fn durations_are_kept_verbatim_so_they_stay_distinct() {
    let h = |d: &str| {
        id(&Rule {
            duration: d.into(),
            ..rule("r", None)
        })
        .hash
    };
    let all = [
        "once",
        "until restart",
        "always",
        "1.5h",
        "2.5h",
        "30s",
        "5m",
        "1h30m",
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(h(a), h(b), "{a} vs {b}");
        }
    }
    // Invalid or over-long strings collapse (and carry no text).
    assert_eq!(h("bad!1"), h("bad!2"));
    assert_eq!(h(&"1".repeat(33)), h("bad!1"));
    assert_ne!(h(&"1".repeat(32)), h("bad!1"));
}

#[test]
fn list_operand_names_match_the_vendored_daemon() {
    let go = include_str!("../../../../vendor/opensnitch/daemon/rule/operator.go");
    let mut vendored: Vec<&str> = go
        .lines()
        .filter_map(|l| l.split("Operand(\"").nth(1))
        .filter_map(|l| l.split('"').next())
        .filter(|n| n.starts_with("lists."))
        .collect();
    vendored.sort_unstable();
    let mut ours = LIST_OPERANDS.to_vec();
    ours.sort_unstable();
    assert_eq!(vendored.len(), 5);
    assert_eq!(vendored, ours);
    for name in vendored {
        let r = rule("r", Some(op("lists", name, "/d")));
        assert!(id(&r).qualifiers.list_contents_not_recorded, "{name}");
        assert!(contains(&id(&r).canonical, "/d"), "{name} directory kept");
    }
}

#[test]
fn a_non_list_operator_cannot_smuggle_data_through_a_structural_kind() {
    for operand in ["true", "list"] {
        let r = rule("r", Some(op("simple", operand, "CANARY")));
        assert!(!contains(&id(&r).canonical, "CANARY"), "{operand}");
    }
}

// ---- class ----------------------------------------------------------

#[test]
fn rule_class_follows_name_prefixes() {
    let cases = [
        ("snitchwatch-default-dns", "snitchwatch-default"),
        ("z00-blocklist:ads:domains", "z00-blocklist"),
        ("my-rule", "user"),
        ("xz00-blocklist:", "user"),
        ("", "user"),
    ];
    for (name, class) in cases {
        let got = id(&rule(name, None)).class.as_str();
        assert_eq!(got, class, "{name}");
        assert!(got.len() <= RULE_CLASS);
    }
}

// ---- bounds ---------------------------------------------------------

fn chain(depth: usize) -> Operator {
    (0..depth).fold(op("simple", "dest.port", "1"), |inner, _| Operator {
        list: vec![inner],
        ..op("list", "list", "")
    })
}

#[test]
fn depth_is_bounded_at_the_cache_limit() {
    use crate::cache::rules::MAX_OPERATOR_DEPTH;
    assert!(identify(&rule("r", Some(chain(MAX_OPERATOR_DEPTH)))).is_ok());
    assert_eq!(
        identify(&rule("r", Some(chain(MAX_OPERATOR_DEPTH + 1)))),
        Err(IdentityError::TooDeep)
    );
}

#[test]
fn hostile_nesting_neither_overflows_the_stack_nor_panics() {
    let hostile = rule("r", Some(chain(5_000)));
    let result = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn_scoped(s, || identify(&hostile))
            .expect("spawn")
            .join()
            .expect("no panic")
    });
    assert_eq!(result, Err(IdentityError::TooDeep));
}

#[test]
fn width_node_count_and_size_are_bounded() {
    use crate::cache::rules::MAX_OPERATOR_LIST_LEN as W;
    let leaf = || op("simple", "dest.port", "1");
    let wide = |n: usize| group((0..n).map(|_| leaf()).collect());
    assert!(identify(&rule("r", Some(wide(W)))).is_ok());
    assert_eq!(
        identify(&rule("r", Some(wide(W + 1)))),
        Err(IdentityError::ListTooLong)
    );
    // 64 x 64 members is within width and depth, over the node bound.
    let tiny = || op("simple", "true", "");
    let bushy = group(
        (0..W)
            .map(|_| group((0..W).map(|_| tiny()).collect()))
            .collect(),
    );
    assert_eq!(
        identify(&rule("r", Some(bushy))),
        Err(IdentityError::TooManyOperators)
    );
    let big = op("simple", "dest.host", &"h".repeat(SNAPSHOT));
    assert_eq!(
        identify(&rule("r", Some(big))),
        Err(IdentityError::TooLarge)
    );
    let near = op("simple", "dest.host", &"h".repeat(SNAPSHOT - 200));
    assert!(identify(&rule("r", Some(near))).is_ok());
    // Many mid-size values add up past the cap.
    let many = group(
        (0..W)
            .map(|_| op("simple", "dest.host", &"h".repeat(200)))
            .collect(),
    );
    assert_eq!(
        identify(&rule("r", Some(many))),
        Err(IdentityError::TooLarge)
    );
    assert_eq!(
        identify(&rule(&"n".repeat(300), None)),
        Err(IdentityError::NameTooLong)
    );
}

#[test]
fn errors_carry_no_rule_text() {
    let e = identify(&rule("CANARY", Some(chain(50)))).unwrap_err();
    assert!(!format!("{e:?} {e}").contains("CANARY"));
}

#[test]
fn the_size_cap_is_exact_for_the_whole_encoding() {
    let at = |n: usize| rule("r", Some(op("simple", "dest.host", &"h".repeat(n))));
    let base = id(&at(0)).canonical.len();
    let fits = SNAPSHOT - base;
    assert_eq!(id(&at(fits)).canonical.len(), SNAPSHOT);
    assert_eq!(identify(&at(fits + 1)), Err(IdentityError::TooLarge));
    // Trailing fixed bytes (sensitive, tag, count) are counted too.
    assert_eq!(identify(&at(SNAPSHOT - 62)), Err(IdentityError::TooLarge));
}

#[test]
fn an_over_long_kind_fails_closed() {
    let long = "k".repeat(MAX_KIND + 1);
    assert_eq!(
        identify(&rule("r", Some(op("simple", &long, "")))),
        Err(IdentityError::KindTooLong)
    );
    let ok = "k".repeat(MAX_KIND);
    assert!(identify(&rule("r", Some(op("simple", &ok, "")))).is_ok());
}
