//! The analysis against the simulator: a rule marked as shadowed must never
//! be the rule the simulator says decides, for any connection. Random rule
//! sets (fixed seed) over a small universe of values chosen to hit the
//! awkward corners: case folding, the long s and Kelvin sign, regexps that
//! lowercase their subject, CIDR containment.

use serde_json::{json, Value};
use snitchwatch_bridge::ws_messages::ServerMessage;

use super::shadow::{analyze, Analysis, FindingKind};
use super::testkit::{regexp, regexp_sensitive, rule, simple, simple_sensitive};
use crate::rules::row_store::{Rule, RulesStore};
use crate::rules::simulator::stops_scan;
use crate::rules::simulator::{simulate, SimulationInput};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const HOSTS: &[&str] = &[
    "example.com",
    "a.example.com",
    "A.EXAMPLE.COM",
    "sub.example.com",
    "k.example.com",
    "b.example.org",
];
const PATHS: &[&str] = &["/usr/bin/curl", "/usr/bin/CURL", "/bin/sh"];
const PORTS: &[&str] = &["80", "443"];
const PROTOCOLS: &[&str] = &["tcp", "TCP", "udp"];
const IPS: &[&str] = &["10.1.2.3", "192.168.1.1"];
const NETWORKS: &[&str] = &["10.0.0.0/8", "10.1.0.0/16", "192.168.0.0/16", "0.0.0.0/0"];
const HOST_PATTERNS: &[&str] = &[
    r"^.*\.example\.com$",
    r"^sub\.example\.com$",
    r"^a\.",
    "example",
    r"^k\.example\.com$",
];
const PATH_PATTERNS: &[&str] = &["curl", "^/usr/"];

/// One of five operands, with a condition on it from a small pool. Rules
/// built from one group collide often enough to shadow each other.
fn atom_in(rng: &mut Rng, group: usize) -> Value {
    match group {
        0 => match rng.below(4) {
            0 => simple("dest.host", rng.pick(HOSTS)),
            1 => simple_sensitive("dest.host", rng.pick(HOSTS)),
            2 => regexp("dest.host", rng.pick(HOST_PATTERNS)),
            _ => regexp_sensitive("dest.host", rng.pick(HOST_PATTERNS)),
        },
        1 => match rng.below(3) {
            0 => simple("process.path", rng.pick(PATHS)),
            1 => simple_sensitive("process.path", rng.pick(PATHS)),
            _ => regexp("process.path", rng.pick(PATH_PATTERNS)),
        },
        2 => simple("dest.port", rng.pick(PORTS)),
        3 => simple("protocol", rng.pick(PROTOCOLS)),
        _ => match rng.below(3) {
            0 => simple("dest.ip", rng.pick(IPS)),
            1 => super::testkit::network("dest.network", rng.pick(NETWORKS)),
            _ => super::testkit::truth(),
        },
    }
}

fn operator_of(members: Vec<Value>) -> Value {
    if members.len() == 1 {
        members.into_iter().next().unwrap()
    } else {
        super::testkit::all_of(members)
    }
}

/// allow, deny, reject, and an action the daemon neither stops on nor allows.
fn random_action(rng: &mut Rng) -> &'static str {
    match rng.below(20) {
        0..=7 => "allow",
        8..=14 => "deny",
        15..=17 => "reject",
        _ => "drop",
    }
}

/// Mostly permanent, sometimes `until restart` or timed.
fn random_duration(rng: &mut Rng) -> &'static str {
    match rng.below(10) {
        0 => "5m",
        1 => "until restart",
        _ => "always",
    }
}

fn random_rule(rng: &mut Rng, name: &str, members: Vec<Value>) -> Rule {
    let action = random_action(rng);
    let mut r = rule(name, action, operator_of(members));
    r.precedence = matches!(action, "allow" | "drop") && rng.below(5) == 0;
    r.duration = random_duration(rng).into();
    r.enabled = rng.below(20) != 0;
    r
}

fn random_rules(rng: &mut Rng) -> Vec<Rule> {
    let count = 2 + rng.below(5);
    let focus = rng.below(5);
    (0..count)
        .map(|i| {
            let members: Vec<Value> = (0..1 + rng.below(3))
                .map(|_| {
                    let group = if rng.below(2) == 0 {
                        focus
                    } else {
                        rng.below(5)
                    };
                    atom_in(rng, group)
                })
                .collect();
            let name = format!("{:03}-r{i}", rng.below(1000));
            random_rule(rng, &name, members)
        })
        .collect()
}

/// Two single-condition rules on the same operand: the cheapest way to meet
/// every pairing of the comparison rules.
fn random_pair(rng: &mut Rng) -> Vec<Rule> {
    let group = rng.below(5);
    (0..2)
        .map(|i| {
            let name = format!("{:03}-p{i}", rng.below(1000));
            let atom = atom_in(rng, group);
            random_rule(rng, &name, vec![atom])
        })
        .collect()
}

/// A rule, a rule that covers it, and a third in between or after: the shape
/// where naming the wrong deciding rule shows.
fn random_triple(rng: &mut Rng) -> Vec<Rule> {
    let group = rng.below(5);
    let shared = atom_in(rng, group);
    let mut rules: Vec<Rule> = (0..3)
        .map(|i| {
            let atoms = if rng.below(3) == 0 {
                vec![atom_in(rng, group)]
            } else {
                vec![shared.clone()]
            };
            let name = format!("{}00-t{i}", rng.below(9));
            random_rule(rng, &name, atoms)
        })
        .collect();
    // Names must be distinct.
    for (i, r) in rules.iter_mut().enumerate() {
        r.name = format!("{}-{}", r.name, i);
    }
    rules
}

fn store(rules: &[Rule]) -> RulesStore {
    let mut store = RulesStore::default();
    store.apply(&ServerMessage::SetRules {
        rules: rules
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect(),
    });
    store
}

fn connections() -> Vec<SimulationInput> {
    // U+017F LATIN SMALL LETTER LONG S and U+212A KELVIN SIGN fold with `s`
    // and `k` for Go's EqualFold.
    let hosts = [
        "example.com",
        "a.example.com",
        "A.EXAMPLE.COM",
        "sub.example.com",
        "SUB.example.com",
        "\u{17F}ub.example.com",
        "k.example.com",
        "\u{212A}.example.com",
        "b.example.org",
    ];
    let mut out = Vec::new();
    for path in PATHS {
        for host in hosts {
            for port in [80u16, 443] {
                for protocol in ["tcp", "udp"] {
                    for ip in ["10.1.2.3", "10.200.0.1", "192.168.1.1", "8.8.8.8"] {
                        out.push(SimulationInput {
                            process_path: Some((*path).into()),
                            dest_host: Some(host.into()),
                            dest_port: port,
                            protocol: Some(protocol.into()),
                            dest_ip: Some(ip.into()),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    out
}

/// Whether `rule` alone matches `input`.
fn matches_alone(rule: &Rule, input: &SimulationInput) -> bool {
    let mut alone = rule.clone();
    alone.enabled = true;
    simulate(&store(&[alone]), input).matched_rule.is_some()
}

/// The name of the rule that decides `input` among `rules`, by name.
fn decider(store: &RulesStore, input: &SimulationInput) -> Option<String> {
    let result = simulate(store, input);
    result.precedence.map(|i| store.rules()[i].name.clone())
}

fn describe(rules: &[Rule]) -> Value {
    json!(rules
        .iter()
        .map(|r| json!({
            "name": r.name, "action": r.action, "precedence": r.precedence,
            "enabled": r.enabled, "duration": r.duration, "operator": r.operator,
        }))
        .collect::<Vec<_>>())
}

/// Counts the findings by kind, after checking, for each, over the universe:
/// - the rule called shadowed never decides;
/// - the named rule matches every connection the shadowed one matches;
/// - and takes precedence on all of them: a stop rule names a decider that
///   stops the scan at or before it, and a non-stop one a decider that stops
///   the scan or is no earlier than it.
fn check_findings(rules: &[Rule], universe: &[SimulationInput], by_kind: &mut [usize; 2]) {
    let Analysis::Done { findings } = analyze(rules) else {
        panic!("small rule sets are analysed");
    };
    if findings.is_empty() {
        return;
    }
    let full = store(rules);
    let mut order: Vec<&Rule> = rules.iter().filter(|r| r.enabled).collect();
    order.sort_by(|a, b| a.name.cmp(&b.name));
    let position = |name: &str| order.iter().position(|r| r.name == name).unwrap();
    for (shadowed, finding) in &findings {
        by_kind[match finding.kind {
            FindingKind::NeverDecides => 0,
            FindingKind::MayBeShadowed => 1,
        }] += 1;
        let b = rules.iter().find(|r| &r.name == shadowed).unwrap();
        let a = rules.iter().find(|r| r.name == finding.by).unwrap();
        let a_stops = stops_scan(a);
        for input in universe {
            let context = || {
                format!(
                    "{shadowed} is called shadowed by {} ({:?}) on {input:?}\n{}",
                    finding.by,
                    finding.kind,
                    describe(rules)
                )
            };
            let decided_by = decider(&full, input);
            assert_ne!(
                decided_by.as_deref(),
                Some(shadowed.as_str()),
                "decides: {}",
                context()
            );
            if !matches_alone(b, input) {
                continue;
            }
            assert!(matches_alone(a, input), "not covered: {}", context());
            let decided_by = decided_by.unwrap_or_else(|| panic!("nothing decides: {}", context()));
            let decider_rule = order[position(&decided_by)];
            let at = position(&decided_by);
            let ok = if a_stops {
                stops_scan(decider_rule) && at <= position(&a.name)
            } else {
                stops_scan(decider_rule) || at >= position(&a.name)
            };
            assert!(
                ok,
                "{decided_by} decides, which does not follow {}: {}",
                a.name,
                context()
            );
        }
    }
}

#[test]
fn a_rule_the_analysis_calls_shadowed_is_covered_and_overridden_in_the_simulator() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let universe = connections();
    let mut by_kind = [0usize; 2];
    for round in 0..500 {
        let rules = match round % 4 {
            0 => random_rules(&mut rng),
            1 => random_triple(&mut rng),
            _ => random_pair(&mut rng),
        };
        check_findings(&rules, &universe, &mut by_kind);
    }
    // The test only means something if the generator finds things to check.
    assert!(by_kind[0] > 30, "findings: {by_kind:?}");
    assert!(by_kind[1] > 0, "may-be-shadowed findings: {by_kind:?}");
}

/// The case random rules rarely reach, pinned against the simulator: Go folds
/// U+017F (long s) with `s`, so the literal matches `ſub.example.com`; the
/// insensitive pattern lowercases its subject and doesn't. The allow decides
/// that connection, so it must not be called shadowed.
#[test]
fn the_long_s_case_is_what_the_simulator_says() {
    let rules = [
        rule(
            "100-pattern",
            "deny",
            regexp("dest.host", r"^sub\.example\.com$"),
        ),
        rule(
            "200-literal",
            "allow",
            simple("dest.host", "sub.example.com"),
        ),
    ];
    let long_s = SimulationInput {
        process_path: Some("/bin/sh".into()),
        dest_host: Some("\u{17F}ub.example.com".into()),
        dest_port: 443,
        protocol: Some("tcp".into()),
        dest_ip: Some("8.8.8.8".into()),
        ..Default::default()
    };
    assert_eq!(
        decider(&store(&rules), &long_s).as_deref(),
        Some("200-literal"),
        "the ground truth the analysis has to respect"
    );
    let mut by_kind = [0; 2];
    check_findings(&rules, &connections(), &mut by_kind);
    assert_eq!(by_kind, [0, 0], "nothing is proven here");
}

/// The review's example: the later allow also never decides, but the deny
/// after it is what decides, so that is the rule named.
#[test]
fn the_rule_that_decides_is_the_one_named() {
    let rules = [
        rule("100-b", "allow", simple("dest.host", "example.com")),
        rule("200-a", "allow", simple("dest.host", "example.com")),
        rule("300-d", "deny", simple("dest.host", "example.com")),
    ];
    let input = SimulationInput {
        process_path: Some("/bin/sh".into()),
        dest_host: Some("example.com".into()),
        dest_port: 443,
        protocol: Some("tcp".into()),
        dest_ip: Some("8.8.8.8".into()),
        ..Default::default()
    };
    assert_eq!(decider(&store(&rules), &input).as_deref(), Some("300-d"));
    let Analysis::Done { findings } = analyze(&rules) else {
        unreachable!()
    };
    assert_eq!(findings["100-b"].by, "300-d");
    assert_eq!(findings["200-a"].by, "300-d");
}
