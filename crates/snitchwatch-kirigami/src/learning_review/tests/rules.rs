//! The rule payloads: the shapes, the stable names, the bridge's policy.

use super::*;
use crate::connections::level_rules::{Level, NodeRef};
use crate::make_rule::add_level_rule_message;
use crate::settings::Settings;
use serde_json::{json, Value};
use snitchwatch_bridge::rule_name::{is_reserved_name, validate_rule_name, MAX_RULE_NAME_BYTES};
use snitchwatch_bridge::rule_policy::{check_wire_rule, PolicyProfile};
use snitchwatch_bridge::translator::verdict::is_answer_name;
use snitchwatch_bridge::ws_messages::ClientMessage;

const PATH: &str = "/usr/bin/curl";

fn program() -> Program {
    prog("curl", PATH, &[("a.example", 3), ("b.example", 2)])
}

fn single(choice: &Choice, program: &Program) -> RulePayload {
    let mut all = rules_for(choice, program).unwrap();
    assert_eq!(all.len(), 1);
    all.remove(0)
}

/// What `make_rule.rs` sends for the same node, as the wire rule.
fn made_by_make_rule(level: Level, key: &str, choice: &str) -> Value {
    let settings = Settings {
        default_duration: "forever".into(),
        ..Settings::default()
    };
    let node = NodeRef {
        program: PATH,
        level,
        key,
    };
    let request = add_level_rule_message(node, choice, &settings, true, 0, "learned-1").unwrap();
    match request.message {
        ClientMessage::AddRule { rule, .. } => rule,
        other => panic!("not an AddRule: {other:?}"),
    }
}

/// The wire rule without what is meant to differ: the name (and the
/// display copy of it), the description and the creation time.
fn masked(mut wire: Value) -> Value {
    for field in ["name", "displayName", "description", "created"] {
        wire[field] = Value::Null;
    }
    wire
}

#[test]
fn any_destination_is_what_make_rule_sends_for_a_program() {
    let payload = single(&Choice::AllowAny, &program());
    assert_eq!(
        masked(payload.to_wire()),
        masked(made_by_make_rule(Level::Program, "", "allow"))
    );
    let wire = payload.to_wire();
    assert_eq!(wire["action"], "allow");
    assert_eq!(wire["duration"], "always");
    assert_eq!(wire["enabled"], true);
    assert_eq!(wire["precedence"], false);
    assert_eq!(
        wire["operator"],
        json!({"type": "simple", "operand": "process.path", "data": PATH, "sensitive": true})
    );
}

#[test]
fn block_is_a_deny_on_the_exact_path_for_any_destination() {
    let payload = single(&Choice::Block, &program());
    assert_eq!(
        masked(payload.to_wire()),
        masked(made_by_make_rule(Level::Program, "", "deny"))
    );
    let wire = payload.to_wire();
    assert_eq!(wire["action"], "deny");
    assert_eq!(wire["duration"], "always");
    assert_eq!(wire["operator"]["operand"], "process.path");
    assert_eq!(wire["operator"]["data"], PATH);
    assert!(wire["operator"].get("operands").is_none(), "no destination");
}

#[test]
fn only_these_destinations_is_one_rule_per_host_like_a_host_rule() {
    let choice = Choice::AllowHosts(vec!["a.example".into(), "B.Example".into()]);
    let payloads = rules_for(&choice, &program()).unwrap();
    assert_eq!(payloads.len(), 2);
    for (payload, host) in payloads.iter().zip(["a.example", "B.Example"]) {
        assert_eq!(
            masked(payload.to_wire()),
            masked(made_by_make_rule(Level::Host, host, "allow")),
            "{host}"
        );
        // Process path AND this host, any port.
        let operands = payload.to_wire()["operator"]["operands"].clone();
        assert_eq!(operands[0]["operand"], "process.path");
        assert_eq!(operands[1]["operand"], "dest.host");
        assert_eq!(operands[1]["data"], host);
        assert_eq!(operands.as_array().map(Vec::len), Some(2));
    }
}

#[test]
fn a_bare_ip_destination_matches_the_address_not_a_host_name() {
    let choice = Choice::AllowHosts(vec!["203.0.113.9".into(), "2001:db8::1".into()]);
    for (payload, ip) in rules_for(&choice, &program())
        .unwrap()
        .iter()
        .zip(["203.0.113.9", "2001:db8::1"])
    {
        let operands = payload.to_wire()["operator"]["operands"].clone();
        assert_eq!(operands[1]["operand"], "dest.ip", "{ip}");
        assert_eq!(operands[1]["data"], ip);
        assert_eq!(
            masked(payload.to_wire()),
            masked(made_by_make_rule(Level::Host, ip, "allow"))
        );
    }
}

#[test]
fn the_directory_rule_is_a_case_sensitive_regexp_on_the_path() {
    let p = prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[("a.example", 2)],
    );
    let rule = directory_rule(&p).unwrap();
    let payload = single(&Choice::DirectoryRule(rule.clone()), &p);
    let wire = payload.to_wire();
    assert_eq!(
        wire["operator"],
        json!({"type": "regexp", "operand": "process.path", "data": rule.pattern, "sensitive": true})
    );
    assert_eq!(wire["action"], "allow");
    assert_eq!(wire["duration"], "always");
    assert_eq!(wire["enabled"], true);
    assert_eq!(wire["precedence"], false);
    assert_eq!(wire["nolog"], false);
}

#[test]
fn every_payload_is_accepted_by_the_bridges_rule_policy() {
    let versioned = prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[("a.example", 2)],
    );
    let mut all: Vec<RulePayload> = Vec::new();
    for program in [&program(), &versioned] {
        for choice in [
            Choice::AllowAny,
            Choice::Block,
            Choice::AllowHosts(vec!["a.example".into(), "203.0.113.9".into()]),
        ] {
            all.extend(rules_for(&choice, program).unwrap());
        }
    }
    all.extend(
        rules_for(
            &Choice::DirectoryRule(directory_rule(&versioned).unwrap()),
            &versioned,
        )
        .unwrap(),
    );
    for payload in &all {
        for profile in [PolicyProfile::Editor, PolicyProfile::Import] {
            let checked = check_wire_rule(&payload.to_wire(), profile);
            assert!(
                checked.is_ok(),
                "{} as {profile:?}: {checked:?}",
                payload.name()
            );
        }
        assert!(!is_reserved_name(payload.name()));
        assert!(!is_answer_name(payload.name()));
        assert!(validate_rule_name(payload.name()).is_ok());
    }
}

#[test]
fn names_are_stable_prefixed_short_and_free_of_path_characters() {
    let all: Vec<RulePayload> = [
        rules_for(&Choice::AllowAny, &program()).unwrap(),
        rules_for(&Choice::Block, &program()).unwrap(),
        rules_for(&Choice::AllowHosts(vec!["a.example".into()]), &program()).unwrap(),
    ]
    .concat();
    for payload in &all {
        let name = payload.name();
        assert!(name.starts_with("snitchwatch-learned-"), "{name}");
        assert!(name.len() <= 26 + 12, "{name}: short");
        assert!(name.len() <= MAX_RULE_NAME_BYTES);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{name}"
        );
        assert!(!name.starts_with("snitchwatch-allow-"), "reserved: {name}");
        assert!(!name.starts_with("snitchwatch-deny-"), "reserved: {name}");
    }
    // The same input, the same name, run after run.
    let again = rules_for(&Choice::AllowAny, &program()).unwrap();
    assert_eq!(again[0].name(), all[0].name());
    assert_eq!(
        rules_for(&Choice::Block, &program()).unwrap()[0].name(),
        all[1].name()
    );
}

#[test]
fn block_names_use_the_block_prefix_and_allow_names_never_do() {
    assert_eq!(BLOCK_NAME_PREFIX, "snitchwatch-learned-block-");
    assert_eq!(ALLOW_NAME_PREFIX, "snitchwatch-learned-");
    let block = single(&Choice::Block, &program());
    assert!(block.name().starts_with(BLOCK_NAME_PREFIX));
    assert_eq!(block.name().len(), BLOCK_NAME_PREFIX.len() + 12);
    let versioned = prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[("a.example", 2)],
    );
    let mut allows = rules_for(&Choice::AllowAny, &program()).unwrap();
    allows.extend(rules_for(&Choice::AllowHosts(vec!["a.example".into()]), &program()).unwrap());
    allows.extend(
        rules_for(
            &Choice::DirectoryRule(directory_rule(&versioned).unwrap()),
            &versioned,
        )
        .unwrap(),
    );
    for allow in allows {
        assert!(allow.name().starts_with(ALLOW_NAME_PREFIX));
        assert!(
            !allow.name().starts_with(BLOCK_NAME_PREFIX),
            "{}",
            allow.name()
        );
        assert_eq!(allow.name().len(), ALLOW_NAME_PREFIX.len() + 12);
    }
}

#[test]
fn names_differ_with_the_kind_the_path_and_the_host() {
    let other = prog("curl", "/usr/bin/wget2", &[("a.example", 3)]);
    let names: Vec<String> = [
        rules_for(&Choice::AllowAny, &program()).unwrap(),
        rules_for(&Choice::Block, &program()).unwrap(),
        rules_for(&Choice::AllowAny, &other).unwrap(),
        rules_for(&Choice::Block, &other).unwrap(),
        rules_for(
            &Choice::AllowHosts(vec!["a.example".into(), "b.example".into()]),
            &program(),
        )
        .unwrap(),
        rules_for(&Choice::AllowHosts(vec!["a.example".into()]), &other).unwrap(),
    ]
    .concat()
    .iter()
    .map(|p| p.name().to_string())
    .collect();
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "{names:?}");
    // The host does not depend on its position in the list.
    let ab = rules_for(
        &Choice::AllowHosts(vec!["a.example".into(), "b.example".into()]),
        &program(),
    )
    .unwrap();
    let ba = rules_for(
        &Choice::AllowHosts(vec!["b.example".into(), "a.example".into()]),
        &program(),
    )
    .unwrap();
    assert_eq!(ab[0].name(), ba[1].name());
    assert_eq!(ab[1].name(), ba[0].name());
}

#[test]
fn the_hash_is_fnv_1a_and_separates_its_parts() {
    use super::super::rule::{fnv1a64, scope_hash};
    // Published FNV-1a 64-bit test vectors.
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    // ("a", "bc") and ("ab", "c") are not the same scope.
    assert_ne!(scope_hash(&["a", "bc"]), scope_hash(&["ab", "c"]));
    assert_ne!(scope_hash(&["a", "b", ""]), scope_hash(&["a", "b"]));
    assert_eq!(scope_hash(&["x"]).len(), 12);
    assert!(scope_hash(&["x"]).bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn a_choice_that_does_not_fit_the_program_makes_no_rule() {
    let bad = nameless("curl", &[]);
    for choice in [
        Choice::AllowAny,
        Choice::Block,
        Choice::AllowHosts(vec!["a.example".into()]),
    ] {
        assert_eq!(
            rules_for(&choice, &bad),
            Err(BuildRefusal::NonSelectable(NonSelectable::NoPath))
        );
    }
    let deleted = prog("x", "/usr/bin/x (deleted)", &[]);
    assert_eq!(
        rules_for(&Choice::Block, &deleted),
        Err(BuildRefusal::NonSelectable(NonSelectable::Deleted))
    );
    let truncated = Program {
        truncated: true,
        ..program()
    };
    assert_eq!(
        rules_for(&Choice::AllowAny, &truncated),
        Err(BuildRefusal::NonSelectable(NonSelectable::Truncated))
    );
    assert_eq!(
        rules_for(&Choice::NonSelectable(NonSelectable::NoPath), &program()),
        Err(BuildRefusal::NotOffered)
    );
    assert_eq!(
        rules_for(&Choice::AllowHosts(vec![]), &program()),
        Err(BuildRefusal::NoHosts)
    );
    for host in ["", "has space", "tab\t", "line\nbreak"] {
        assert_eq!(
            rules_for(&Choice::AllowHosts(vec![host.into()]), &program()),
            Err(BuildRefusal::BadHost),
            "{host:?}"
        );
    }
}

#[test]
fn a_directory_rule_that_is_not_the_programs_own_is_refused() {
    let p = prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[],
    );
    let mut forged = directory_rule(&p).unwrap();
    forged.pattern = "^/.*$".to_string();
    assert_eq!(
        rules_for(&Choice::DirectoryRule(forged), &p),
        Err(BuildRefusal::NoDirectoryRule)
    );
    let plain = program();
    let own = directory_rule(&p).unwrap();
    assert_eq!(
        rules_for(&Choice::DirectoryRule(own), &plain),
        Err(BuildRefusal::NoDirectoryRule)
    );
}

#[test]
fn hostile_paths_make_exact_rules_that_are_still_valid() {
    for path in [
        "/home/u/a b/c",
        "/opt/ünï/中文/app",
        "/opt/a.b+c(d)[e]{f}^$|/app",
        "/opt/quote\"and'/app",
        "/opt/back\\slash/app",
    ] {
        let p = prog("app", path, &[("a.example", 2)]);
        let payload = single(&Choice::AllowAny, &p);
        assert_eq!(payload.to_wire()["operator"]["data"], path);
        assert!(
            check_wire_rule(&payload.to_wire(), PolicyProfile::Editor).is_ok(),
            "{path}"
        );
    }
}

#[test]
fn building_a_payload_does_not_send_or_change_anything() {
    let p = program();
    let before = p.clone();
    let _ = rules_for(&Choice::Block, &p);
    assert_eq!(p, before);
}
