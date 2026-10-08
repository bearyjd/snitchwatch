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

fn atom(rng: &mut Rng) -> Value {
    match rng.below(9) {
        0 => simple("dest.host", rng.pick(HOSTS)),
        1 => simple_sensitive("dest.host", rng.pick(HOSTS)),
        2 => regexp("dest.host", rng.pick(HOST_PATTERNS)),
        3 => regexp_sensitive("dest.host", rng.pick(HOST_PATTERNS)),
        4 => simple("process.path", rng.pick(PATHS)),
        5 => regexp("process.path", rng.pick(PATH_PATTERNS)),
        6 => simple("dest.port", rng.pick(PORTS)),
        7 => simple("protocol", rng.pick(PROTOCOLS)),
        _ => match rng.below(3) {
            0 => simple("dest.ip", rng.pick(IPS)),
            1 => super::testkit::network("dest.network", rng.pick(NETWORKS)),
            _ => super::testkit::truth(),
        },
    }
}

fn random_rules(rng: &mut Rng) -> Vec<Rule> {
    let count = 2 + rng.below(4);
    (0..count)
        .map(|i| {
            let members: Vec<Value> = (0..1 + rng.below(3)).map(|_| atom(rng)).collect();
            let operator = if members.len() == 1 {
                members.into_iter().next().unwrap()
            } else {
                super::testkit::all_of(members)
            };
            let name = format!("{:03}-r{i}", rng.below(1000));
            let action = if rng.below(2) == 0 { "allow" } else { "deny" };
            let mut r = rule(&name, action, operator);
            r.precedence = action == "allow" && rng.below(5) == 0;
            if rng.below(10) == 0 {
                r.duration = "5m".into();
            }
            r.enabled = rng.below(20) != 0;
            r
        })
        .collect()
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

#[test]
fn a_rule_the_analysis_calls_shadowed_never_decides_in_the_simulator() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let universe = connections();
    let mut by_kind = [0usize; 3];
    for round in 0..120 {
        let rules = random_rules(&mut rng);
        let Analysis::Done { findings } = analyze(&rules) else {
            panic!("small rule sets are analysed");
        };
        if findings.is_empty() {
            continue;
        }
        let store = store(&rules);
        for (shadowed, finding) in &findings {
            by_kind[match finding.kind {
                FindingKind::Redundant => 0,
                FindingKind::NeverApplies => 1,
                FindingKind::MayBeShadowed => 2,
            }] += 1;
            for input in &universe {
                let result = simulate(&store, input);
                assert_ne!(
                    result.matched_rule.as_deref(),
                    Some(shadowed.as_str()),
                    "round {round}: {shadowed} is called shadowed by {} ({:?}) but decides {input:?}\n{}",
                    finding.by,
                    finding.kind,
                    json!(rules.iter().map(|r| json!({
                        "name": r.name, "action": r.action, "precedence": r.precedence,
                        "enabled": r.enabled, "duration": r.duration, "operator": r.operator,
                    })).collect::<Vec<_>>()),
                );
            }
        }
    }
    // The test only means something if the generator finds things to check.
    assert!(by_kind[0] > 10, "redundant findings: {by_kind:?}");
    assert!(by_kind[1] > 10, "never-applies findings: {by_kind:?}");
    assert!(by_kind[2] > 0, "may-be-shadowed findings: {by_kind:?}");
}
