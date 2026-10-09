//! The opt-in DNS entry for the system resolver (owner decision S6, issue
//! #117): `/usr/lib/systemd/systemd-resolved` to any address, port 53, over
//! TCP and UDP. The one entry with no destination condition, so these tests
//! pin that nothing else gets that shape and that the rule matches only what
//! it says.
//!
//! [`daemon_matches`] mirrors opensnitchd v1.8.0's `Operator.Compile` and
//! `Match` (`vendor:daemon/rule/operator.go`) for the leaves a curated rule
//! uses, so "matches only" is checked against the rule `entries()` builds,
//! not against a copy of its pattern.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use snitchwatch_proto::protocol::{Operator, Rule};

use super::canonical::{canonical, is_unedited};
use super::reconcile::{plan, CuratedAction, DaemonRules, EntryStatus};
use super::store::Choices;
use super::*;
use crate::rule_policy::{self, PolicyProfile};

const ID: &str = "dns-resolved";
const NAME: &str = "snitchwatch-default-dns-resolved";
const PATH: &str = "/usr/lib/systemd/systemd-resolved";
const PROTOCOL_PATTERN: &str = "^(tcp|udp)6?$";

fn dns() -> &'static CuratedEntry {
    entries()
        .iter()
        .find(|entry| entry.id == ID)
        .expect("the DNS entry")
}

/// A connection, as the daemon hands it to `Operator.Match`.
struct Conn {
    path: &'static str,
    host: &'static str,
    ip: &'static str,
    port: u16,
    protocol: &'static str,
}

impl Conn {
    /// resolved asking some server on port 53 over UDP.
    fn lookup() -> Self {
        Self {
            path: PATH,
            host: "",
            ip: "192.168.1.1",
            port: 53,
            protocol: "udp",
        }
    }
}

/// opensnitchd's `Operator.Match` for `operator`: a list ANDs its members; a
/// `simple` leaf is `==` when sensitive and `EqualFold` otherwise; a
/// `regexp` leaf is an unanchored search, with the pattern and the value
/// lowercased when not sensitive (`Compile`, `reCmp`).
fn daemon_matches(operator: &Operator, conn: &Conn) -> bool {
    if operator.r#type == "list" {
        return operator.list.iter().all(|leaf| daemon_matches(leaf, conn));
    }
    let value = match operator.operand.as_str() {
        "process.path" => conn.path.to_string(),
        "dest.host" => conn.host.to_string(),
        "dest.ip" => conn.ip.to_string(),
        "dest.port" => conn.port.to_string(),
        "protocol" => conn.protocol.to_string(),
        other => panic!("the mirror doesn't know the operand {other}"),
    };
    match operator.r#type.as_str() {
        "simple" if operator.sensitive => value == operator.data,
        "simple" => value.to_lowercase() == operator.data.to_lowercase(),
        "regexp" => {
            let (pattern, value) = if operator.sensitive {
                (operator.data.clone(), value)
            } else {
                (operator.data.to_lowercase(), value.to_lowercase())
            };
            Regex::new(&pattern).unwrap().is_match(&value)
        }
        other => panic!("the mirror doesn't know the type {other}"),
    }
}

fn matches(conn: &Conn) -> bool {
    daemon_matches(dns().rule().operator.as_ref().unwrap(), conn)
}

/// opensnitchd's report of `rule` after a round trip: its own `created`, the
/// list's operand `list` and its JSON left in `data`.
fn as_reported(rule: &Rule) -> Rule {
    let mut reported = rule.clone();
    reported.created = 1_700_000_000;
    let op = reported.operator.as_mut().unwrap();
    op.operand = "list".into();
    op.data = r#"[{"type":"simple"}]"#.into();
    reported
}

fn daemon(rules: &[Rule]) -> BTreeMap<String, Rule> {
    rules
        .iter()
        .map(|rule| (rule.name.clone(), rule.clone()))
        .collect()
}

fn reconcile(rules: &BTreeMap<String, Rule>, choices: &Choices) -> super::reconcile::Plan {
    let none = BTreeSet::new();
    plan(
        entries(),
        DaemonRules {
            rules,
            left_out: &none,
            files_left: &none,
            maybe_applied: &none,
        },
        choices,
    )
}

fn leaves(rule: &Rule) -> Vec<(&str, &str, &str, bool)> {
    rule.operator
        .as_ref()
        .unwrap()
        .list
        .iter()
        .map(|op| {
            (
                op.r#type.as_str(),
                op.operand.as_str(),
                op.data.as_str(),
                op.sensitive,
            )
        })
        .collect()
}

#[test]
fn the_entry_is_offered_last_with_the_reviewed_name_and_words() {
    assert_eq!(entries().last().map(|e| e.id.as_str()), Some(ID));
    let entry = dns();
    assert_eq!(entry.rule_name(), NAME);
    assert_eq!(entry.path, PATH);
    assert_eq!(entry.port, 53);
    assert_eq!(
        entry.allows(),
        "/usr/lib/systemd/systemd-resolved may connect to any address on TCP and UDP port 53, \
         over IPv4 and IPv6."
    );
    assert_eq!(
        entry.why,
        "With the firewall set to deny by default, no name can be looked up until the system's \
         DNS resolver may reach its DNS server. That server differs on every network, so this \
         lets that one program reach any address on port 53. It does not make lookups private: \
         whoever runs the server sees them. NetworkManager's check, Flathub updates and \
         blocklist downloads need names looked up, so they need this."
    );
    // Honest about its evidence: udp was captured, tcp is the fallback.
    assert!(entry.evidence.contains("udp") && entry.evidence.contains("not captured"));
}

#[test]
fn the_rule_is_the_exact_program_port_53_and_both_transports_and_nothing_else() {
    let rule = dns().rule();
    assert_eq!(
        leaves(&rule),
        [
            ("simple", "process.path", PATH, true),
            ("simple", "dest.port", "53", false),
            ("regexp", "protocol", PROTOCOL_PATTERN, false),
        ]
    );
    assert_eq!(
        (
            rule.action.as_str(),
            rule.duration.as_str(),
            rule.precedence,
            rule.nolog,
            rule.description.as_str()
        ),
        ("allow", "always", false, false, DESCRIPTION)
    );
    check_curated_rule(&rule).unwrap();
}

#[test]
fn it_matches_resolver_traffic_to_any_address_on_port_53_over_tcp_or_udp() {
    for ip in [
        "1.1.1.1",
        "192.168.1.1",
        "10.0.2.3",
        "::1",
        "fe80::1",
        "127.0.0.53",
    ] {
        for protocol in ["udp", "tcp", "udp6", "tcp6", "UDP", "Tcp6"] {
            let conn = Conn {
                ip,
                protocol,
                ..Conn::lookup()
            };
            assert!(matches(&conn), "{ip} {protocol}");
        }
    }
    // A name for the server doesn't matter; the rule never reads it.
    let named = Conn {
        host: "dns.example.net",
        ..Conn::lookup()
    };
    assert!(matches(&named));
}

#[test]
fn it_matches_no_other_program_port_or_protocol() {
    assert!(matches(&Conn::lookup()), "the baseline matches");
    // (what changes, program, port, protocol), each one change from a lookup.
    let others: [(&str, &str, u16, &str); 28] = [
        // Other programs, including near misses of the exact path.
        ("curl", "/usr/bin/curl", 53, "udp"),
        ("empty path", "", 53, "udp"),
        ("case", "/USR/LIB/SYSTEMD/SYSTEMD-RESOLVED", 53, "udp"),
        ("case 2", "/usr/lib/systemd/systemd-Resolved", 53, "udp"),
        ("no d", "/usr/lib/systemd/systemd-resolve", 53, "udp"),
        ("suffix", "/usr/lib/systemd/systemd-resolved-x", 53, "udp"),
        ("prefix", "x/usr/lib/systemd/systemd-resolved", 53, "udp"),
        (
            "local",
            "/usr/local/lib/systemd/systemd-resolved",
            53,
            "udp",
        ),
        ("lib alias", "/lib/systemd/systemd-resolved", 53, "udp"),
        (
            "deleted",
            "/usr/lib/systemd/systemd-resolved (deleted)",
            53,
            "udp",
        ),
        ("networkd", "/usr/lib/systemd/systemd-networkd", 53, "udp"),
        // Other ports, including near misses of 53.
        ("dot", PATH, 853, "udp"),
        ("mdns", PATH, 5353, "udp"),
        ("llmnr", PATH, 5355, "udp"),
        ("5", PATH, 5, "udp"),
        ("530", PATH, 530, "udp"),
        ("153", PATH, 153, "udp"),
        ("52", PATH, 52, "udp"),
        ("https", PATH, 443, "tcp"),
        ("zero", PATH, 0, "udp"),
        // Other protocols, including near misses of the pattern.
        ("udplite", PATH, 53, "udplite"),
        ("sctp", PATH, 53, "sctp"),
        ("icmp", PATH, 53, "icmp"),
        ("tcpx", PATH, 53, "tcpx"),
        ("xudp", PATH, 53, "xudp"),
        ("tcp66", PATH, 53, "tcp66"),
        ("tcpudp", PATH, 53, "tcpudp"),
        ("empty protocol", PATH, 53, ""),
    ];
    for (what, path, port, protocol) in others {
        let conn = Conn {
            path,
            port,
            protocol,
            ..Conn::lookup()
        };
        assert!(!matches(&conn), "{what} matched");
    }
}

#[test]
fn no_other_entry_matches_a_destination_it_does_not_name() {
    // The DNS entry is the only one with no destination condition.
    let open: Vec<&str> = entries()
        .iter()
        .filter(|entry| {
            !leaves(&entry.rule())
                .iter()
                .any(|leaf| leaf.1 == "dest.host" || leaf.1 == "dest.ip")
        })
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(open, [ID]);
    // And no other entry's rule matches resolver traffic.
    for entry in entries().iter().filter(|entry| entry.id != ID) {
        let operator = entry.rule().operator.unwrap();
        assert!(!daemon_matches(&operator, &Conn::lookup()), "{}", entry.id);
    }
}

#[test]
fn the_allowlist_takes_only_the_exact_dns_shape() {
    let base = dns().rule();
    let changed = |change: &dyn Fn(&mut Rule)| {
        let mut rule = base.clone();
        change(&mut rule);
        check_curated_rule(&rule)
    };
    let leaf_at = |index: usize, change: &dyn Fn(&mut Operator)| {
        changed(&|rule: &mut Rule| change(&mut rule.operator.as_mut().unwrap().list[index]))
    };
    assert!(changed(&|_| {}).is_ok());
    assert!(changed(&|rule| rule.name = "snitchwatch-default-dns".into()).is_ok());
    assert!(changed(&|rule| rule.precedence = true).is_err());
    assert!(changed(&|rule| rule.action = "deny".into()).is_err());
    assert!(changed(&|rule| rule.duration = "until restart".into()).is_err());
    assert!(changed(&|rule| rule.description = "mine".into()).is_err());
    // The program: this one, exact and case sensitive. A destination-less
    // rule for any other program is refused.
    for other in [
        "/usr/bin/curl",
        "/usr/lib/systemd/systemd-resolve",
        "/usr/bin/flatpak",
    ] {
        assert!(leaf_at(0, &|op| op.data = other.into()).is_err(), "{other}");
    }
    assert!(leaf_at(0, &|op| op.sensitive = false).is_err());
    assert!(leaf_at(0, &|op| op.r#type = "regexp".into()).is_err());
    assert!(leaf_at(0, &|op| {
        op.r#type = "regexp".into();
        op.data = "^/usr/lib/systemd/.*$".into();
    })
    .is_err());
    // The port: 53 only.
    for port in [
        "0", "5", "530", "5353", "853", "443", "53-54", "+53", "053", "53 ", "1-65535",
    ] {
        assert!(leaf_at(1, &|op| op.data = port.into()).is_err(), "{port}");
    }
    assert!(leaf_at(1, &|op| op.r#type = "regexp".into()).is_err());
    // The transports: exactly TCP and UDP, anchored.
    for pattern in [
        "^.*$",
        "^(tcp|udp)6?",
        "(tcp|udp)6?$",
        "^(tcp|udp)$",
        "^(tcp|udp|sctp)6?$",
        "^(tcp|udp|udplite)6?$",
        "^tcp6?$",
        "^udp6?$",
        "tcp|udp",
    ] {
        assert!(
            leaf_at(2, &|op| op.data = pattern.into()).is_err(),
            "{pattern}"
        );
    }
    assert!(leaf_at(2, &|op| op.r#type = "simple".into()).is_err());
    // Nothing else: no extra, missing or reordered condition.
    assert!(changed(&|rule| {
        let extra = leaf("simple", "process.command", "resolved", false);
        rule.operator.as_mut().unwrap().list.push(extra);
    })
    .is_err());
    assert!(changed(&|rule| {
        let any = leaf("regexp", "dest.ip", "^.*$", false);
        rule.operator.as_mut().unwrap().list.insert(1, any);
    })
    .is_err());
    assert!(changed(&|rule| {
        rule.operator.as_mut().unwrap().list.remove(1);
    })
    .is_err());
    assert!(changed(&|rule| {
        rule.operator.as_mut().unwrap().list.remove(2);
    })
    .is_err());
    assert!(changed(&|rule| {
        rule.operator.as_mut().unwrap().list.swap(1, 2);
    })
    .is_err());
}

#[test]
fn a_host_or_this_computer_entry_cannot_use_the_both_transports_pattern() {
    let flatpak = entries()
        .iter()
        .find(|entry| entry.id == "flatpak-flathub")
        .unwrap();
    let mut rule = flatpak.rule();
    check_curated_rule(&rule).unwrap();
    rule.operator.as_mut().unwrap().list[3].data = PROTOCOL_PATTERN.into();
    assert!(check_curated_rule(&rule).is_err());
}

#[test]
fn only_the_resolver_can_be_offered_with_no_destination() {
    let file = |entry: &str| format!(r#"{{"version": 1, "entries": [{entry}]}}"#);
    let entry = |path: &str, port: u16, protocol: &str, rest: &str| {
        format!(
            r#"{{"id": "x", "path": "{path}", "port": {port}, "protocol": "{protocol}", "why": "w", "evidence": "e"{rest}}}"#
        )
    };
    let any = r#", "anyAddress": true"#;
    assert!(parse(&file(&entry(PATH, 53, "tcp+udp", any))).is_ok());
    for bad in [
        // Any address for another program, port or transport.
        entry("/usr/bin/curl", 53, "tcp+udp", any),
        entry("/usr/bin/flatpak", 443, "tcp", any),
        entry(PATH, 443, "tcp+udp", any),
        entry(PATH, 853, "tcp+udp", any),
        entry(PATH, 53, "tcp", any),
        entry(PATH, 53, "udp", any),
        // Any address together with a host or this computer.
        entry(
            PATH,
            53,
            "tcp+udp",
            r#", "anyAddress": true, "host": "a.org""#,
        ),
        entry(
            PATH,
            53,
            "tcp+udp",
            r#", "anyAddress": true, "loopback": true"#,
        ),
        // Several destinations, where the rule alone would pass as one of
        // them while `allows()` says another.
        entry(
            "/usr/bin/chronyc",
            323,
            "udp",
            r#", "anyAddress": true, "loopback": true"#,
        ),
        entry(
            "/usr/bin/flatpak",
            443,
            "tcp",
            r#", "anyAddress": true, "host": "dl.flathub.org""#,
        ),
        // No destination at all is still refused, and so is "false".
        entry(PATH, 53, "tcp+udp", ""),
        entry(PATH, 53, "tcp+udp", r#", "anyAddress": false"#),
        // The both-transports value for a host or this computer.
        entry(PATH, 53, "tcp+udp", r#", "host": "a.org""#),
        entry("/usr/bin/chronyc", 323, "tcp+udp", r#", "loopback": true"#),
        // An unknown spelling of the field or the value.
        entry(PATH, 53, "tcp+udp", r#", "any_address": true"#),
        entry(PATH, 53, "tcpudp", any),
        entry(PATH, 53, "both", any),
    ] {
        assert!(parse(&file(&bad)).is_err(), "{bad}");
    }
}

#[test]
fn it_is_off_by_default_and_turned_on_installs_exactly_its_rule() {
    let nothing = reconcile(&BTreeMap::new(), &Choices::default());
    assert!(nothing.actions.is_empty(), "{:?}", nothing.actions);
    assert_eq!(nothing.statuses[ID], EntryStatus::Off);
    assert_eq!(nothing.choices, Choices::default());

    let on = Choices::default().enable(ID);
    let first = reconcile(&BTreeMap::new(), &on);
    assert_eq!(first.actions, [CuratedAction::Install(ID.into())]);
    assert_eq!(first.statuses[ID], EntryStatus::Installing);

    // Another entry turned on doesn't install this one.
    let flatpak_only = Choices::default().enable("flatpak-flathub");
    let plan = reconcile(&BTreeMap::new(), &flatpak_only);
    assert_eq!(
        plan.actions,
        [CuratedAction::Install("flatpak-flathub".into())]
    );
    assert_eq!(plan.statuses[ID], EntryStatus::Off);

    let confirmed = on.installed(ID, &dns().rule());
    let after = reconcile(&daemon(&[as_reported(&dns().rule())]), &confirmed);
    assert!(after.actions.is_empty(), "{:?}", after.actions);
    assert_eq!(after.statuses[ID], EntryStatus::Installed);
}

#[test]
fn a_first_run_with_the_rule_already_in_the_firewall_changes_nothing() {
    let rules = daemon(&[as_reported(&dns().rule())]);
    let plan = reconcile(&rules, &Choices::default());
    assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    assert_eq!(plan.statuses[ID], EntryStatus::InFirewall);
}

#[test]
fn a_user_delete_is_respected_and_an_edit_is_left_alone() {
    let installed = Choices::default().enable(ID).installed(ID, &dns().rule());
    // Deleted outside Snitchwatch: never reinstalled, across restarts too.
    let gone = reconcile(&BTreeMap::new(), &installed);
    assert!(gone.actions.is_empty(), "{:?}", gone.actions);
    assert_eq!(gone.statuses[ID], EntryStatus::DeletedOutside);
    let again = reconcile(&BTreeMap::new(), &gone.choices);
    assert!(again.actions.is_empty());
    assert_eq!(again.statuses[ID], EntryStatus::DeletedOutside);

    // Edited, in each place it could be widened: left alone, never
    // overwritten, never deleted, even on opt-out.
    let edits: [fn(&mut Rule); 5] = [
        |r| r.operator.as_mut().unwrap().list[1].data = "5353".into(),
        |r| r.operator.as_mut().unwrap().list[0].data = "/usr/bin/curl".into(),
        |r| r.operator.as_mut().unwrap().list[2].data = "^.*$".into(),
        |r| {
            r.operator.as_mut().unwrap().list.remove(2);
        },
        |r| r.precedence = true,
    ];
    for edit in edits {
        let mut edited = dns().rule();
        edit(&mut edited);
        let rules = daemon(&[edited.clone()]);
        let on = reconcile(&rules, &installed);
        assert!(on.actions.is_empty(), "{edited:?}");
        assert_eq!(on.statuses[ID], EntryStatus::EditedByYou);
        let off = reconcile(&rules, &installed.disable(ID));
        assert!(off.actions.is_empty(), "{edited:?}");
        assert_eq!(off.statuses[ID], EntryStatus::EditedByYou);
        assert!(!is_unedited(Some(dns()), None, &edited));
    }

    // Off and unedited (a pure toggle isn't an edit): deleted.
    let toggled = Rule {
        enabled: false,
        ..as_reported(&dns().rule())
    };
    let off = reconcile(&daemon(&[toggled]), &installed.disable(ID));
    assert_eq!(
        off.actions,
        [CuratedAction::Delete {
            id: ID.into(),
            name: NAME.into()
        }]
    );
}

#[test]
fn the_daemons_report_of_the_rule_is_unedited() {
    let reported = as_reported(&dns().rule());
    assert!(is_unedited(Some(dns()), None, &reported));
    assert_eq!(canonical(&reported), canonical(&dns().rule()));
    assert!(toggleable(&reported));
    let off = Rule {
        enabled: false,
        ..reported
    };
    assert!(toggleable(&off), "a toggle isn't an edit");
}

#[test]
fn the_reserved_name_keeps_users_from_adding_widening_or_deleting_it() {
    let rule = dns().rule();
    // A GUI can't add or import a rule under the name, even the shipped one.
    let problems = rule_policy::validate_user_rule(&rule, PolicyProfile::Editor).unwrap_err();
    assert!(problems.iter().any(|p| p.path == "name"), "{problems:?}");
    // Listed read-only with the shipped-default reason; not deletable from
    // the Rules page; only the unedited copy can be toggled.
    assert_eq!(
        rule_policy::read_only_reason(&rule),
        Some(rule_policy::CURATED_DEFAULT_REASON)
    );
    assert!(rule_policy::toggleable(&rule));
    assert!(!rule_policy::deletable(&rule));
    let mut edited = rule.clone();
    edited.operator.as_mut().unwrap().list[1].data = "5353".into();
    assert_eq!(
        rule_policy::read_only_reason(&edited),
        Some(rule_policy::CURATED_MANAGED_REASON)
    );
    assert!(!rule_policy::toggleable(&edited));
    assert!(!rule_policy::deletable(&edited));
}

#[test]
fn the_wire_summary_carries_the_bridges_own_words() {
    let summary = crate::curated::wire::CuratedDefaultSummary {
        id: dns().id.clone(),
        program: dns().path.clone(),
        allows: dns().allows(),
        why: dns().why.clone(),
        on: false,
        status: EntryStatus::Off,
        problem: None,
    };
    let json = serde_json::to_value(&summary).unwrap();
    assert_eq!(json["id"], ID);
    assert_eq!(json["program"], PATH);
    assert_eq!(
        json["allows"],
        "/usr/lib/systemd/systemd-resolved may connect to any address on TCP and UDP port 53, \
         over IPv4 and IPv6."
    );
    assert_eq!(json["on"], false);
    assert!(json["why"]
        .as_str()
        .unwrap()
        .contains("does not make lookups private"));
}
