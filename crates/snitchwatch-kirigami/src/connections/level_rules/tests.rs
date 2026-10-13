use super::*;
use snitchwatch_bridge::translator::verdict::verdict_to_rule;
use snitchwatch_bridge::ws_messages::VerdictDuration;

const PROG: &str = "/usr/bin/curl";

fn node<'a>(level: Level, key: &'a str) -> NodeRef<'a> {
    NodeRef {
        program: PROG,
        level,
        key,
    }
}

/// A rule the bridge itself makes for a real connection (so the matcher is
/// checked against the bridge, not against its own expectations).
fn bridge_rule(
    name: &str,
    verdict: Verdict,
    scope: VerdictScope,
    host: &str,
    ip: &str,
    path: &str,
) -> Rule {
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: host.into(),
        dst_ip: ip.into(),
        dst_port: 443,
        process_path: path.into(),
        ..Default::default()
    };
    let made = verdict_to_rule(verdict, VerdictDuration::Always, scope, &conn, 0).unwrap();
    let mut wire = rule_to_wire(&made);
    wire["name"] = name.into();
    serde_json::from_value(wire).unwrap()
}

fn with_rules(rules: &[Rule]) -> LevelRules {
    let mut level_rules = LevelRules::default();
    let wire = rules
        .iter()
        .map(|r| {
            let mut wire = serde_json::to_value(r).unwrap();
            // Bridge-set fields are never sent back, so `to_value` drops them.
            wire["deletable"] = r.deletable.into();
            wire
        })
        .collect();
    level_rules.apply(&ServerMessage::SetRules { rules: wire });
    level_rules
}

fn state(rules: &[Rule], node: NodeRef<'_>) -> RuleState {
    with_rules(rules).project(node).state
}

#[test]
fn each_level_maps_to_its_verdict_scope() {
    assert_eq!(
        resolve(node(Level::Program, "")).scope,
        VerdictScope::AnyHost
    );
    assert_eq!(
        resolve(node(Level::Domain, "example.com")).scope,
        VerdictScope::AnyHostOnDomain
    );
    assert_eq!(
        resolve(node(Level::Host, "www.example.com")).scope,
        VerdictScope::ThisHost
    );
    for (level, key) in [
        (Level::Program, ""),
        (Level::Domain, "example.com"),
        (Level::Host, "www.example.com"),
    ] {
        let target = resolve(node(level, key));
        assert_eq!(target.effective, level, "{level:?}");
        assert!(target.bindable && !target.too_broad && target.operator.is_some());
    }
    assert_eq!(
        [Level::Program, Level::Domain, Level::Host].map(|l| l as i32),
        [0, 1, 2],
        "the numbers the groupLevel role reports"
    );
    assert_eq!(Level::from_i32(3), None);
    assert_eq!(Level::from_i32(-1), None);
}

#[test]
fn a_bare_ip_degrades_to_the_host_level_and_matches_dest_ip() {
    for level in [Level::Domain, Level::Host] {
        let target = resolve(node(level, "93.184.216.34"));
        assert_eq!(target.effective, Level::Host, "{level:?}");
        assert!(!target.too_broad);
        let text = target.operator.unwrap().to_string();
        assert!(
            text.contains("dest.ip") && text.contains("93.184.216.34"),
            "{text}"
        );
    }
    let rule = bridge_rule(
        "ip",
        Verdict::Deny,
        VerdictScope::ThisHost,
        "",
        "93.184.216.34",
        PROG,
    );
    assert_eq!(
        state(&[rule], node(Level::Domain, "93.184.216.34")),
        RuleState::Deny
    );
}

#[test]
fn a_public_suffix_degrades_and_is_too_broad_to_cover() {
    for key in ["co.uk", "github.io"] {
        let target = resolve(node(Level::Domain, key));
        assert_eq!(target.effective, Level::Host, "{key}");
        assert!(target.too_broad, "{key}");
        assert!(target.operator.is_none(), "{key}");
    }
    // The registrable domain under a two-part suffix is fine.
    let target = resolve(node(Level::Domain, "example.co.uk"));
    assert_eq!(target.effective, Level::Domain);
    assert!(!target.too_broad);
}

#[test]
fn a_single_label_host_degrades_to_itself() {
    let target = resolve(node(Level::Domain, "localhost"));
    assert_eq!(target.effective, Level::Host);
    assert!(!target.too_broad);
    assert_eq!(target.scope, VerdictScope::ThisHost);
    let text = target.operator.unwrap().to_string();
    assert!(
        text.contains("dest.host") && text.contains("localhost"),
        "{text}"
    );
}

#[test]
fn an_apex_only_domain_still_gets_a_domain_rule() {
    let target = resolve(node(Level::Domain, "example.com"));
    assert_eq!(target.effective, Level::Domain);
    let text = target.operator.unwrap().to_string();
    assert!(
        text.contains("regexp") && text.contains("example\\\\.com"),
        "{text}"
    );
}

#[test]
fn a_program_without_a_path_cannot_be_bound() {
    for program in ["curl", "", "/proc/1/exe"] {
        let target = resolve(NodeRef {
            program,
            level: Level::Host,
            key: "example.com",
        });
        assert!(!target.bindable, "{program:?}");
        assert!(target.operator.is_none());
        assert_eq!(
            with_rules(&[])
                .project(NodeRef {
                    program,
                    level: Level::Host,
                    key: "example.com"
                })
                .state,
            RuleState::None
        );
    }
}

#[test]
fn rules_the_bridge_makes_are_recognised_at_their_level() {
    let program = bridge_rule(
        "p",
        Verdict::Allow,
        VerdictScope::AnyHost,
        "x.com",
        "",
        PROG,
    );
    let domain = bridge_rule(
        "d",
        Verdict::Deny,
        VerdictScope::AnyHostOnDomain,
        "www.example.com",
        "",
        PROG,
    );
    // Original case, as a prompt keeps it; the host key is lowercased.
    let host = bridge_rule(
        "h",
        Verdict::Allow,
        VerdictScope::ThisHost,
        "WWW.Example.com",
        "",
        PROG,
    );
    let all = [program, domain, host];
    let projection = |node| with_rules(&all).project(node);
    let p = projection(node(Level::Program, ""));
    assert_eq!(
        (p.state, p.names),
        (RuleState::Allow, vec!["p".to_string()])
    );
    let d = projection(node(Level::Domain, "example.com"));
    assert_eq!((d.state, d.names), (RuleState::Deny, vec!["d".to_string()]));
    let h = projection(node(Level::Host, "www.example.com"));
    assert_eq!(
        (h.state, h.names),
        (RuleState::Allow, vec!["h".to_string()])
    );
    // Another host, another domain: nothing there.
    assert_eq!(
        projection(node(Level::Host, "api.example.com")).state,
        RuleState::None
    );
    assert_eq!(
        projection(node(Level::Domain, "other.org")).state,
        RuleState::None
    );
}

#[test]
fn a_rule_for_another_program_is_not_this_programs() {
    let other = bridge_rule(
        "o",
        Verdict::Deny,
        VerdictScope::ThisHost,
        "example.com",
        "",
        "/usr/bin/wget",
    );
    assert_eq!(
        state(&[other], node(Level::Host, "example.com")),
        RuleState::None
    );
}

#[test]
fn a_wildcard_for_a_sub_domain_is_not_the_domains_rule() {
    // `a.b.example.com` wildcards `b.example.com`, not `example.com`.
    let sub = bridge_rule(
        "s",
        Verdict::Deny,
        VerdictScope::AnyHostOnDomain,
        "a.b.example.com",
        "",
        PROG,
    );
    assert_eq!(
        state(&[sub], node(Level::Domain, "example.com")),
        RuleState::None
    );
}

#[test]
fn allow_and_deny_at_one_level_are_partial() {
    let allow = bridge_rule(
        "a",
        Verdict::Allow,
        VerdictScope::ThisHost,
        "example.com",
        "",
        PROG,
    );
    let deny = bridge_rule(
        "b",
        Verdict::Deny,
        VerdictScope::ThisHost,
        "example.com",
        "",
        PROG,
    );
    let projection = with_rules(&[allow, deny]).project(node(Level::Host, "example.com"));
    assert_eq!(projection.state, RuleState::Partial);
    assert_eq!(projection.names, ["a", "b"]);
}

#[test]
fn narrower_rules_make_the_parent_partial() {
    let host = bridge_rule(
        "h",
        Verdict::Deny,
        VerdictScope::ThisHost,
        "www.example.com",
        "",
        PROG,
    );
    let apex = bridge_rule(
        "e",
        Verdict::Deny,
        VerdictScope::ThisHost,
        "example.com",
        "",
        PROG,
    );
    let domain = bridge_rule(
        "d",
        Verdict::Allow,
        VerdictScope::AnyHostOnDomain,
        "www.example.org",
        "",
        PROG,
    );
    for rules in [vec![host], vec![apex]] {
        let level_rules = with_rules(&rules);
        let p = level_rules.project(node(Level::Domain, "example.com"));
        assert_eq!((p.state, p.names.len()), (RuleState::Partial, 0));
        assert_eq!(
            level_rules.project(node(Level::Program, "")).state,
            RuleState::Partial
        );
        // Narrower rules never make a sibling domain or the host itself partial.
        assert_eq!(
            level_rules
                .project(node(Level::Domain, "example.org"))
                .state,
            RuleState::None
        );
        assert_eq!(
            level_rules
                .project(node(Level::Host, "mail.example.com"))
                .state,
            RuleState::None
        );
    }
    assert_eq!(
        state(&[domain], node(Level::Program, "")),
        RuleState::Partial
    );
}

#[test]
fn disabled_other_sourced_and_odd_action_rules_do_not_count() {
    let make = |name: &str| {
        bridge_rule(
            name,
            Verdict::Deny,
            VerdictScope::ThisHost,
            "example.com",
            "",
            PROG,
        )
    };
    let host = node(Level::Host, "example.com");
    let off = Rule {
        enabled: false,
        ..make("off")
    };
    let blocklist = make("z00-blocklist:abc:hosts");
    let odd = Rule {
        action: "weird".into(),
        ..make("odd")
    };
    assert_eq!(state(&[off, blocklist, odd], host), RuleState::None);
}

#[test]
fn only_deletable_rules_are_offered_for_deleting() {
    let rule = Rule {
        deletable: Some(false),
        ..bridge_rule(
            "fixed",
            Verdict::Deny,
            VerdictScope::ThisHost,
            "example.com",
            "",
            PROG,
        )
    };
    let p = with_rules(&[rule]).project(node(Level::Host, "example.com"));
    assert_eq!(p.state, RuleState::Deny);
    assert!(p.names.is_empty());
}

fn result(request_id: &str, outcome: RuleCommandOutcome) -> ServerMessage {
    ServerMessage::RuleCommandResult {
        request_id: request_id.into(),
        outcome,
    }
}

#[test]
fn a_command_in_flight_is_pending_until_its_result() {
    let host = node(Level::Host, "example.com");
    let mut level_rules = with_rules(&[]);
    let operator = level_rules.target(host).operator.clone().unwrap();
    level_rules.begin_create("make-1", host, operator);
    assert_eq!(level_rules.project(host).state, RuleState::Pending);
    assert_eq!(
        level_rules.project(node(Level::Host, "other.com")).state,
        RuleState::None
    );
    // Someone else's result changes nothing.
    assert!(level_rules
        .apply(&result("make-9", RuleCommandOutcome::Ok))
        .finished
        .is_none());
    assert_eq!(level_rules.project(host).state, RuleState::Pending);
    let done = level_rules
        .apply(&result("make-1", RuleCommandOutcome::Ok))
        .finished
        .unwrap();
    assert_eq!(done.ending, Ending::Created);
    assert!(done.finished.saved);
    assert_eq!(done.finished.status, "The rule was created.");
    assert_eq!(level_rules.project(host).state, RuleState::None);
}

#[test]
fn a_refused_create_says_why_and_is_not_created() {
    let host = node(Level::Host, "example.com");
    let mut level_rules = with_rules(&[]);
    level_rules.begin_create(
        "make-1",
        host,
        level_rules.target(host).operator.clone().unwrap(),
    );
    let done = level_rules
        .apply(&result(
            "make-1",
            RuleCommandOutcome::Rejected {
                reason: "a rule with this name exists".into(),
            },
        ))
        .finished
        .unwrap();
    assert_eq!(done.ending, Ending::NotCreated);
    assert!(!done.finished.saved);
    assert_eq!(
        done.finished.status,
        "The rule wasn't created: a rule with this name exists"
    );
}

#[test]
fn a_delete_in_flight_is_pending_and_reports_in_delete_words() {
    let host = node(Level::Host, "example.com");
    let rule = bridge_rule(
        "r",
        Verdict::Deny,
        VerdictScope::ThisHost,
        "example.com",
        "",
        PROG,
    );
    let mut level_rules = with_rules(&[rule]);
    assert_eq!(level_rules.project(host).state, RuleState::Deny);
    level_rules.begin_delete("del-1", host, "r");
    assert_eq!(level_rules.project(host).state, RuleState::Pending);
    let done = level_rules
        .apply(&result("del-1", RuleCommandOutcome::Ok))
        .finished
        .unwrap();
    assert_eq!(done.finished.status, "The rule was deleted.");
    // The rule list says when it is gone, not the delete's send.
    assert_eq!(level_rules.project(host).state, RuleState::Deny);
    let gone = level_rules.apply(&ServerMessage::SetRules { rules: vec![] });
    assert!(gone.rules_changed);
    assert_eq!(level_rules.project(host).state, RuleState::None);
}

#[test]
fn silence_or_a_lost_session_ends_a_pending_command_as_unknown() {
    let host = node(Level::Host, "example.com");
    let mut level_rules = with_rules(&[]);
    let operator = level_rules.target(host).operator.clone().unwrap();
    level_rules.begin_create("make-1", host, operator.clone());
    let started = Instant::now();
    let wait = Duration::from_secs(30);
    assert!(level_rules.expire(started, wait).is_empty(), "not yet");
    assert_eq!(level_rules.project(host).state, RuleState::Pending);
    let ended = level_rules.expire(started + wait + Duration::from_secs(1), wait);
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].ending, Ending::Unknown);
    assert!(!ended[0].finished.saved);
    assert_eq!(ended[0].finished.status, crate::make_rule::NO_ANSWER);
    assert_eq!(level_rules.project(host).state, RuleState::None);

    level_rules.begin_create("make-2", host, operator.clone());
    let ended = level_rules.abandon_all();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].ending, Ending::Unknown);
    level_rules.begin_create("make-3", host, operator);
    level_rules.abandon("make-3");
    assert_eq!(level_rules.project(host).state, RuleState::None);
}

#[test]
fn a_domain_with_a_host_under_a_delegated_suffix_is_too_broad() {
    let levels = LevelRules::default();
    let domain = node(Level::Domain, "amazonaws.com");
    // `s3.amazonaws.com` is a public suffix: `bucket.s3.amazonaws.com` is
    // its own registrable root, and `*.amazonaws.com` would cover it.
    let hosts = ["bucket.s3.amazonaws.com", "ec2.amazonaws.com"];
    let p = levels.project_in(domain, hosts);
    assert!(p.too_broad);
    assert_eq!(p.state, RuleState::None);
    // The same domain without such a host is fine.
    assert!(!levels.project_in(domain, ["ec2.amazonaws.com"]).too_broad);
}

#[test]
fn an_ordinary_subdomain_does_not_make_a_domain_too_broad() {
    let levels = LevelRules::default();
    let domain = node(Level::Domain, "github.com");
    let p = levels.project_in(domain, ["api.github.com", "GitHub.com", "x.y.github.com"]);
    assert!(!p.too_broad);
    // Other levels and bare IPs never take the check.
    let host = node(Level::Host, "bucket.s3.amazonaws.com");
    assert!(
        !levels
            .project_in(host, ["bucket.s3.amazonaws.com"])
            .too_broad
    );
    let ip = node(Level::Domain, "93.184.216.34");
    assert!(!levels.project_in(ip, ["93.184.216.34"]).too_broad);
}

#[test]
fn a_delegated_domain_still_shows_a_rule_that_already_exists() {
    let rule = bridge_rule(
        "wild",
        Verdict::Allow,
        VerdictScope::AnyHostOnDomain,
        "x.amazonaws.com",
        "",
        PROG,
    );
    let p = with_rules(&[rule]).project_in(
        node(Level::Domain, "amazonaws.com"),
        ["bucket.s3.amazonaws.com"],
    );
    assert!(p.too_broad);
    assert_eq!((p.state, p.matching), (RuleState::Allow, 1));
}
