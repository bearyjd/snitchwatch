//! `add_level_rule_message`: the tree's rule buttons.
use super::*;
use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
use snitchwatch_bridge::translator::upstream::UpstreamEffect;

const PROG: &str = "/usr/bin/curl";

fn node<'a>(level: Level, key: &'a str) -> NodeRef<'a> {
    NodeRef {
        program: PROG,
        level,
        key,
    }
}

fn settings(duration: &str) -> Settings {
    Settings {
        default_duration: duration.into(),
        ..Settings::default()
    }
}

fn make(
    node: NodeRef<'_>,
    choice: &str,
    duration: &str,
    confirmed: bool,
) -> Result<LevelRuleRequest, LevelRuleRefusal> {
    add_level_rule_message(
        node,
        choice,
        &settings(duration),
        confirmed,
        1_700_000_000_123,
        "level-1",
    )
}

fn wire_rule(request: &LevelRuleRequest) -> serde_json::Value {
    match &request.message {
        ClientMessage::AddRule { rule, .. } => rule.clone(),
        other => panic!("expected AddRule, got {other:?}"),
    }
}

#[test]
fn each_level_makes_a_program_bound_rule_of_its_scope() {
    let cases = [
        (Level::Program, "", "any-host"),
        (Level::Domain, "example.com", "regexp"),
        (Level::Host, "www.example.com", "dest.host"),
    ];
    for (level, key, expect) in cases {
        let request = make(node(level, key), "deny", "forever", true).unwrap();
        let rule = wire_rule(&request);
        assert_eq!(rule["action"], "deny", "{level:?}");
        let text = rule["operator"].to_string();
        assert!(
            text.contains(PROG) && text.contains("process.path"),
            "{text}"
        );
        if level != Level::Program {
            assert!(text.contains(expect), "{level:?}: {text}");
        } else {
            assert_eq!(rule["operator"]["operand"], "process.path", "{text}");
        }
        // What was made is what the state projection looks for.
        assert_eq!(
            crate::connections::level_rules::resolve(node(level, key))
                .operator
                .as_ref(),
            Some(&request.operator),
            "{level:?}"
        );
        let ClientMessage::AddRule {
            rule, request_id, ..
        } = request.message
        else {
            unreachable!()
        };
        assert_eq!(request_id.as_deref(), Some("level-1"));
        notification_for_effect(&UpstreamEffect::AddRule { rule }, 1)
            .expect("the bridge accepts the rule")
            .expect("as a CHANGE_RULE");
    }
}

#[test]
fn the_rule_is_named_for_the_node_and_cannot_replace_a_prompts_rule() {
    use snitchwatch_bridge::rule_name::{is_reserved_name, validate_rule_name};
    let names: Vec<String> = [
        node(Level::Program, ""),
        node(Level::Domain, "example.com"),
        node(Level::Host, "example.com"),
    ]
    .into_iter()
    .map(|n| {
        wire_rule(&make(n, "allow", "forever", true).unwrap())["name"]
            .as_str()
            .unwrap()
            .to_owned()
    })
    .collect();
    for name in &names {
        assert!(name.ends_with("-made-1700000000123"), "{name}");
        validate_rule_name(name).unwrap();
        assert!(!is_reserved_name(name));
    }
    assert_eq!(
        names.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
}

#[test]
fn the_duration_comes_from_settings() {
    for (token, daemon) in [
        ("for_5_minutes", "5m"),
        ("until_restart", "until restart"),
        ("forever", "always"),
        ("until_quit", "until restart"),
    ] {
        let rule =
            wire_rule(&make(node(Level::Host, "example.com"), "allow", token, true).unwrap());
        assert_eq!(rule["duration"], daemon, "{token}");
    }
    // An unknown default is the normal default (forever), never once-only.
    let rule =
        wire_rule(&make(node(Level::Host, "example.com"), "allow", "garbage", true).unwrap());
    assert_eq!(rule["duration"], "always");
}

#[test]
fn a_once_only_default_makes_no_rule_and_says_so() {
    let refusal = make(node(Level::Host, "example.com"), "allow", "this_time", true).unwrap_err();
    assert_eq!(refusal, LevelRuleRefusal::NotRemembered);
    assert!(refusal.reason().contains("Settings"));
}

#[test]
fn the_program_level_is_refused_without_confirmation() {
    let refusal = make(node(Level::Program, ""), "deny", "forever", false).unwrap_err();
    assert_eq!(refusal, LevelRuleRefusal::NeedsConfirmation);
    assert!(make(node(Level::Program, ""), "deny", "forever", true).is_ok());
    // Confirmation changes nothing for the narrower levels.
    for confirmed in [false, true] {
        assert!(make(
            node(Level::Domain, "example.com"),
            "deny",
            "forever",
            confirmed
        )
        .is_ok());
        assert!(make(
            node(Level::Host, "example.com"),
            "deny",
            "forever",
            confirmed
        )
        .is_ok());
    }
}

#[test]
fn an_unbindable_program_is_refused_with_a_reason() {
    for program in ["curl", "", "Kernel connection", "/proc/1/exe"] {
        for (level, key) in [
            (Level::Program, ""),
            (Level::Domain, "example.com"),
            (Level::Host, "example.com"),
        ] {
            let node = NodeRef {
                program,
                level,
                key,
            };
            let refusal = make(node, "deny", "forever", true).unwrap_err();
            assert_eq!(
                refusal,
                LevelRuleRefusal::UnknownProgram,
                "{program:?} {level:?}"
            );
            assert!(refusal.reason().contains("program"), "{}", refusal.reason());
        }
    }
}

#[test]
fn a_public_suffix_domain_is_refused_but_a_bare_ip_and_localhost_are_not() {
    assert_eq!(
        make(node(Level::Domain, "co.uk"), "deny", "forever", true).unwrap_err(),
        LevelRuleRefusal::TooBroad
    );
    let ip = wire_rule(
        &make(
            node(Level::Domain, "93.184.216.34"),
            "deny",
            "forever",
            true,
        )
        .unwrap(),
    );
    assert!(ip["operator"].to_string().contains("dest.ip"));
    let local =
        wire_rule(&make(node(Level::Domain, "localhost"), "deny", "forever", true).unwrap());
    assert!(local["operator"].to_string().contains("dest.host"));
}

#[test]
fn a_bad_choice_or_request_id_makes_nothing() {
    let host = node(Level::Host, "example.com");
    assert_eq!(
        make(host, "maybe", "forever", true).unwrap_err(),
        LevelRuleRefusal::UnknownChoice
    );
    for bad in ["", "no spaces", &"x".repeat(65)] {
        let refusal =
            add_level_rule_message(host, "deny", &settings("forever"), true, 0, bad).unwrap_err();
        assert_eq!(refusal, LevelRuleRefusal::BadRequestId, "{bad:?}");
    }
}
