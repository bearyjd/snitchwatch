//! The opt-in DNS entry for the system resolver (owner decision S6, issue
//! #117): `/usr/lib/systemd/systemd-resolved`, run by the `systemd-resolve`
//! account, to any address, port 53, over TCP and UDP. The one entry with no
//! destination condition, so these tests pin that nothing else gets that
//! shape, that the rule matches only what it says, and that it names its
//! sender: a path alone is not enough, since any user can run that binary
//! with `LD_PRELOAD`.
//!
//! [`daemon_matches`] mirrors opensnitchd v1.8.0's `Operator.Compile` and
//! `Match` (`vendor:daemon/rule/operator.go`) for the leaves a curated rule
//! uses, so "matches only" is checked against the rule `entries()` builds,
//! not against a copy of its pattern.

use regex::Regex;
use snitchwatch_proto::protocol::{Operator, Rule};

use super::*;

pub(super) const ID: &str = "dns-resolved";
pub(super) const NAME: &str = "snitchwatch-default-dns-resolved";
pub(super) const PATH: &str = "/usr/lib/systemd/systemd-resolved";
/// The `systemd-resolve` account's user ID on Fedora (its `sysusers.d`
/// fixes 193).
pub(super) const UID: u32 = 193;
const PROTOCOL_PATTERN: &str = "^(tcp|udp)6?$";

pub(super) fn dns() -> &'static CuratedEntry {
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
    uid: u32,
}

impl Conn {
    /// resolved, as its own account, asking some server on port 53 over UDP.
    fn lookup() -> Self {
        Self {
            path: PATH,
            host: "",
            ip: "192.168.1.1",
            port: 53,
            protocol: "udp",
            uid: UID,
        }
    }
}

/// The daemon's `Process.CleanPath`: a path read from a replaced binary
/// loses its ` (deleted)` suffix first (`procmon/details.go`).
fn clean_path(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

/// opensnitchd's `Operator.Match` for `operator`: a list ANDs its members; a
/// `simple` leaf is `==` when sensitive and `EqualFold` otherwise; a
/// `regexp` leaf is an unanchored search, with the pattern and the value
/// lowercased when not sensitive (`Compile`, `reCmp`). `user.id` is the
/// socket's host user ID (`Connection.Entry.UserId`), which a user
/// namespace cannot change.
fn daemon_matches(operator: &Operator, conn: &Conn) -> bool {
    if operator.r#type == "list" {
        return operator.list.iter().all(|leaf| daemon_matches(leaf, conn));
    }
    let value = match operator.operand.as_str() {
        "process.path" => clean_path(conn.path).to_string(),
        "user.id" => conn.uid.to_string(),
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

pub(super) fn leaves(rule: &Rule) -> Vec<(&str, &str, &str, bool)> {
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
         over IPv4 and IPv6, but only while it runs as user ID 193 (the systemd-resolve account)."
    );
    assert_eq!(
        entry.why,
        "A deny default blocks every lookup until the system resolver may reach its DNS server. \
         That server differs on every network, so this lets it reach any address on port 53. \
         Lookups are not private: whoever runs the server sees them, and any program can send \
         data out inside lookups, since every app's lookups use this rule. Flathub updates, \
         blocklist downloads and NetworkManager's check need it."
    );
    assert!(entry.why.chars().count() <= 400);
    // Honest about its evidence: udp was captured, tcp is the fallback.
    assert!(entry.evidence.contains("udp") && entry.evidence.contains("not captured"));
}

#[test]
fn the_rule_is_the_exact_program_its_account_port_53_and_both_transports_and_nothing_else() {
    let rule = dns().rule();
    // `user.id`, not `user.name`: Compile rewrites a `user.name` leaf's data
    // to the uid and the daemon then saves it that way, so after a daemon
    // restart the file's "193" would be looked up as a name and fail.
    assert_eq!(
        leaves(&rule),
        [
            ("simple", "process.path", PATH, true),
            ("simple", "user.id", "193", false),
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
    // The daemon strips " (deleted)" from a path first, so a binary replaced
    // on disk (only root can do that under /usr) still matches. Not
    // reassurance: the rule names a path, and the account is the guard.
    let replaced = Conn {
        path: "/usr/lib/systemd/systemd-resolved (deleted)",
        ..Conn::lookup()
    };
    assert!(matches(&replaced));
}

#[test]
fn it_matches_no_other_program_port_protocol_or_user() {
    assert!(matches(&Conn::lookup()), "the baseline matches");
    // (what changes, program, port, protocol, user ID), each one change
    // from a lookup.
    let others: [(&str, &str, u16, &str, u32); 34] = [
        // The same binary run by anyone else: any user can run it with
        // LD_PRELOAD and get a process whose path is exactly this one.
        ("user", PATH, 53, "udp", 1000),
        ("root", PATH, 53, "udp", 0),
        ("uid below", PATH, 53, "udp", 192),
        ("uid above", PATH, 53, "tcp", 194),
        ("uid digits", PATH, 53, "udp", 1930),
        ("uid prefix", PATH, 53, "udp", 19),
        ("nobody", PATH, 53, "tcp6", 65534),
        // Other programs, including near misses of the exact path.
        ("curl", "/usr/bin/curl", 53, "udp", UID),
        ("empty path", "", 53, "udp", UID),
        ("case", "/USR/LIB/SYSTEMD/SYSTEMD-RESOLVED", 53, "udp", UID),
        (
            "case 2",
            "/usr/lib/systemd/systemd-Resolved",
            53,
            "udp",
            UID,
        ),
        ("no d", "/usr/lib/systemd/systemd-resolve", 53, "udp", UID),
        (
            "suffix",
            "/usr/lib/systemd/systemd-resolved-x",
            53,
            "udp",
            UID,
        ),
        (
            "prefix",
            "x/usr/lib/systemd/systemd-resolved",
            53,
            "udp",
            UID,
        ),
        (
            "local",
            "/usr/local/lib/systemd/systemd-resolved",
            53,
            "udp",
            UID,
        ),
        (
            "networkd",
            "/usr/lib/systemd/systemd-networkd",
            53,
            "udp",
            UID,
        ),
        // Other ports, including near misses of 53.
        ("dot", PATH, 853, "udp", UID),
        ("mdns", PATH, 5353, "udp", UID),
        ("llmnr", PATH, 5355, "udp", UID),
        ("5", PATH, 5, "udp", UID),
        ("530", PATH, 530, "udp", UID),
        ("153", PATH, 153, "udp", UID),
        ("52", PATH, 52, "udp", UID),
        ("https", PATH, 443, "tcp", UID),
        ("zero", PATH, 0, "udp", UID),
        // Other protocols, including near misses of the pattern.
        ("udplite", PATH, 53, "udplite", UID),
        ("sctp", PATH, 53, "sctp", UID),
        ("icmp", PATH, 53, "icmp", UID),
        ("tcpx", PATH, 53, "tcpx", UID),
        ("xudp", PATH, 53, "xudp", UID),
        ("tcp66", PATH, 53, "tcp66", UID),
        ("tcpudp", PATH, 53, "tcpudp", UID),
        ("empty protocol", PATH, 53, "", UID),
        // Several differences at once.
        ("user and port", PATH, 5353, "udp", 1000),
    ];
    for (what, path, port, protocol, uid) in others {
        let conn = Conn {
            path,
            port,
            protocol,
            uid,
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
    // And no other entry's rule matches resolver traffic, whoever sends it.
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
    assert!(changed(&|rule| rule.name = "dns-resolved".into()).is_err());
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
    // The sender: the resolver's own account, as a user ID, exactly. A
    // destination-less rule without it, or for another user, is refused.
    for uid in [
        "0", "1000", "192", "194", "1930", "19", "+193", "0193", "193 ", "", "193-194",
    ] {
        assert!(
            leaf_at(1, &|op| op.data = uid.into()).is_err(),
            "uid {uid:?}"
        );
    }
    assert!(leaf_at(1, &|op| {
        op.operand = "user.name".into();
        op.data = "systemd-resolve".into();
    })
    .is_err());
    // `user.name` even with the number: Compile would rewrite the name to
    // the uid and the saved file could not be loaded again.
    assert!(leaf_at(1, &|op| op.operand = "user.name".into()).is_err());
    assert!(leaf_at(1, &|op| op.operand = "process.id".into()).is_err());
    assert!(leaf_at(1, &|op| op.r#type = "regexp".into()).is_err());
    assert!(leaf_at(1, &|op| {
        op.r#type = "regexp".into();
        op.data = "^.*$".into();
    })
    .is_err());
    assert!(leaf_at(1, &|op| op.sensitive = true).is_err());
    assert!(changed(&|rule| {
        rule.operator.as_mut().unwrap().list.remove(1);
    })
    .is_err());
    // The port: 53 only.
    for port in [
        "0", "5", "530", "5353", "853", "443", "53-54", "+53", "053", "53 ", "1-65535",
    ] {
        assert!(leaf_at(2, &|op| op.data = port.into()).is_err(), "{port}");
    }
    assert!(leaf_at(2, &|op| op.r#type = "regexp".into()).is_err());
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
            leaf_at(3, &|op| op.data = pattern.into()).is_err(),
            "{pattern}"
        );
    }
    assert!(leaf_at(3, &|op| op.r#type = "simple".into()).is_err());
    // Nothing else: no extra, missing, repeated or reordered condition.
    assert!(changed(&|rule| {
        let extra = leaf("simple", "process.command", "resolved", false);
        rule.operator.as_mut().unwrap().list.push(extra);
    })
    .is_err());
    assert!(changed(&|rule| {
        let again = leaf("simple", "user.id", "193", false);
        rule.operator.as_mut().unwrap().list.push(again);
    })
    .is_err());
    assert!(changed(&|rule| {
        let any = leaf("regexp", "dest.ip", "^.*$", false);
        rule.operator.as_mut().unwrap().list.insert(1, any);
    })
    .is_err());
    for gone in [2, 3] {
        assert!(changed(&|rule| {
            rule.operator.as_mut().unwrap().list.remove(gone);
        })
        .is_err());
    }
    for (a, b) in [(1, 2), (2, 3), (1, 3), (0, 1)] {
        assert!(changed(&|rule| {
            rule.operator.as_mut().unwrap().list.swap(a, b);
        })
        .is_err());
    }
    // The pin at the end of the list isn't the shape either.
    assert!(changed(&|rule| {
        let list = &mut rule.operator.as_mut().unwrap().list;
        let pin = list.remove(1);
        list.push(pin);
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
fn a_host_entry_cannot_borrow_the_dns_accounts_pin() {
    // A user pin is the DNS shape's own: a host entry with one, in any
    // place, is not a curated rule.
    let flatpak = entries()
        .iter()
        .find(|entry| entry.id == "flatpak-flathub")
        .unwrap();
    for index in [1, 2, 4] {
        let mut rule = flatpak.rule();
        let pin = leaf("simple", "user.id", "193", false);
        rule.operator.as_mut().unwrap().list.insert(index, pin);
        assert!(check_curated_rule(&rule).is_err(), "pin at {index}");
    }
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
