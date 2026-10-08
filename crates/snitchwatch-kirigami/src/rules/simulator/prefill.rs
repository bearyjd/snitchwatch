//! "Simulate this connection": filling the simulator form from a connection
//! row.
//!
//! A row carries a process path (when the bridge knows it), the destination
//! host (with a caveat, see [`daemon_dst_host`]), IP and port, and the
//! protocol — nothing else the simulator can use.
//! Everything it doesn't carry stays **unknown**: a blank form field, which
//! [`SimulationForm::to_input`] turns into `None`, never an empty string that
//! would be compared as a real value.

use snitchwatch_bridge::ws_messages::ConnectionRow;

use super::SimulationForm;

/// The protocol names opensnitchd gives a connection (`conman/connection.go`
/// `parseDirection`; the `6` suffix is IPv6). The simulator sheet offers
/// exactly these; a row's protocol outside the list is treated as unknown
/// rather than invented into the form.
pub const DAEMON_PROTOCOLS: [&str; 10] = [
    "tcp", "tcp6", "udp", "udp6", "udplite", "udplite6", "sctp", "sctp6", "icmp", "icmp6",
];

/// What a row says about the `DstHost` opensnitchd had for its connection.
enum DaemonHost {
    /// A host name.
    Named(String),
    /// None: the empty `DstHost` of a connection to a bare IP.
    Empty,
    /// The row can't say.
    Unknown,
}

/// The port a DNS query goes to. For TCP and UDP the daemon copies the
/// query's question name into `DstHost` (`conman/connection.go`,
/// `getDomains`).
const DNS_PORT: u16 = 53;

/// Whether opensnitchd may have copied a DNS question into `DstHost`: only
/// for TCP and UDP (`parseDirection`), so not for the protocols known not to.
fn may_carry_a_dns_question(protocol: &str) -> bool {
    !matches!(
        protocol,
        "udplite" | "udplite6" | "sctp" | "sctp6" | "icmp" | "icmp6"
    )
}

/// The `DstHost` opensnitchd had for `row`'s connection.
///
/// The bridge's `connection_to_row` (`translator/connection.rs`) puts the IP
/// in `dst_host` when the daemon's `DstHost` is empty, which is what a
/// connection to a bare IP has (`conman/connection.go`:
/// `DstHost: dns.HostOr(ip, "")`). A name resolved from the DNS cache is never
/// the IP literal (`dns/track.go` drops `resolved == hostname`), so `dst_host`
/// equal to `dst_ip` is normally that stand-in and the daemon's host was the
/// empty string.
///
/// The exception is a DNS query: the question name is copied into `DstHost`,
/// and a query for an IP-literal name (`dig 1.1.1.1 @1.1.1.1`) really has
/// `DstHost == DstIP`. The row can't tell the two apart, so on port 53 the
/// host is unknown rather than guessed either way.
fn daemon_dst_host(row: &ConnectionRow, protocol: &str) -> DaemonHost {
    let ip_stands_in = !row.dst_ip.is_empty() && row.dst_host == row.dst_ip;
    if ip_stands_in && row.dst_port == DNS_PORT && may_carry_a_dns_question(protocol) {
        DaemonHost::Unknown
    } else if ip_stands_in || row.dst_host.is_empty() {
        DaemonHost::Empty
    } else {
        DaemonHost::Named(row.dst_host.clone())
    }
}

impl SimulationForm {
    /// The form for simulating `row`: its known fields set, the rest blank.
    pub fn for_connection(row: &ConnectionRow) -> Self {
        let protocol = row.protocol.trim().to_ascii_lowercase();
        let (dest_host, dest_host_empty) = match daemon_dst_host(row, &protocol) {
            DaemonHost::Named(host) => (host, false),
            DaemonHost::Empty => (String::new(), true),
            DaemonHost::Unknown => (String::new(), false),
        };
        Self {
            // Blank when the bridge doesn't know it; never the process name.
            process_path: row.process_path.clone().unwrap_or_default(),
            dest_host,
            dest_host_empty,
            dest_ip: row.dst_ip.clone(),
            dest_port: i64::from(row.dst_port),
            protocol: if DAEMON_PROTOCOLS.contains(&protocol.as_str()) {
                protocol
            } else {
                String::new()
            },
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::row_store::RulesStore;
    use crate::rules::simulator::{simulate, SimulationInput};
    use snitchwatch_bridge::translator::connection::connection_to_row;
    use snitchwatch_bridge::ws_messages::ServerMessage;
    use snitchwatch_proto::protocol::Connection;

    /// What opensnitchd sends for a connection to github.com.
    fn daemon_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            dst_host: "github.com".to_string(),
            dst_ip: "140.82.112.3".to_string(),
            dst_port: 443,
            process_path: "/usr/bin/curl".to_string(),
            ..Default::default()
        }
    }

    /// The row the bridge makes of `conn`, by its real translator, so the
    /// tests see the rows the Connections page really holds.
    fn row_of(conn: &Connection) -> ConnectionRow {
        connection_to_row(conn, 1)
    }

    fn row() -> ConnectionRow {
        row_of(&daemon_connection())
    }

    /// A store holding one enabled allow rule per `(name, type, data)`, each
    /// on `dest.host`.
    fn store_of_host_rules(rules: &[(&str, &str, &str)]) -> RulesStore {
        let rules = rules
            .iter()
            .map(|(name, kind, data)| {
                serde_json::json!({
                    "name": name, "enabled": true, "action": "allow",
                    "duration": "always", "description": "", "precedence": false,
                    "operator": {"type": kind, "operand": "dest.host", "data": data,
                                 "sensitive": false, "list": []}
                })
            })
            .collect();
        let mut store = RulesStore::new();
        store.apply(&ServerMessage::SetRules { rules });
        store
    }

    fn input(row: &ConnectionRow) -> SimulationInput {
        SimulationForm::for_connection(row).to_input()
    }

    #[test]
    fn the_fields_a_row_carries_are_filled_in() {
        let form = SimulationForm::for_connection(&row());
        assert_eq!(form.process_path, "/usr/bin/curl");
        assert_eq!(form.dest_host, "github.com");
        assert_eq!(form.dest_ip, "140.82.112.3");
        assert_eq!(form.dest_port, 443);
        assert_eq!(form.protocol, "tcp");
    }

    #[test]
    fn everything_else_stays_unknown() {
        let expected = SimulationInput {
            process_path: Some("/usr/bin/curl".to_string()),
            dest_host: Some("github.com".to_string()),
            dest_port: 443,
            protocol: Some("tcp".to_string()),
            dest_ip: Some("140.82.112.3".to_string()),
            ..Default::default()
        };
        // Parent paths, command, ids, environment, source address, interfaces
        // and checksums are all `None`, not empty.
        assert_eq!(input(&row()), expected);
    }

    #[test]
    fn an_unknown_process_path_is_not_the_process_name() {
        let unknown = ConnectionRow {
            process_path: None,
            ..row()
        };
        let form = SimulationForm::for_connection(&unknown);
        assert_eq!(form.process_path, "");
        assert_eq!(input(&unknown).process_path, None);
    }

    #[test]
    fn a_blank_or_spaces_only_path_is_unknown() {
        for path in ["", "   "] {
            let blank = ConnectionRow {
                process_path: Some(path.to_string()),
                ..row()
            };
            assert_eq!(input(&blank).process_path, None, "{path:?}");
        }
    }

    #[test]
    fn an_unknown_destination_ip_stays_unknown() {
        let unknown = ConnectionRow {
            dst_ip: String::new(),
            ..row()
        };
        assert_eq!(input(&unknown).dest_ip, None);
    }

    #[test]
    fn an_unknown_protocol_never_becomes_an_empty_one() {
        for protocol in ["", "  ", "gre", "<b>tcp</b>"] {
            let odd = ConnectionRow {
                protocol: protocol.to_string(),
                ..row()
            };
            assert_eq!(
                SimulationForm::for_connection(&odd).protocol,
                "",
                "{protocol:?}"
            );
            assert_eq!(input(&odd).protocol, None, "{protocol:?}");
        }
    }

    #[test]
    fn every_protocol_the_daemon_names_is_kept() {
        for protocol in DAEMON_PROTOCOLS {
            let named = ConnectionRow {
                protocol: protocol.to_string(),
                ..row()
            };
            assert_eq!(input(&named).protocol.as_deref(), Some(protocol));
        }
        // The daemon's names are lowercase; a differently cased one is the
        // same protocol.
        let loud = ConnectionRow {
            protocol: "TCP6".to_string(),
            ..row()
        };
        assert_eq!(input(&loud).protocol.as_deref(), Some("tcp6"));
    }

    /// A connection to `ip` on `port` with no host name from the daemon, as the
    /// bridge's row shows it (the IP stands in for the missing host).
    fn ip_only(ip: &str, port: u32, protocol: &str) -> ConnectionRow {
        let row = row_of(&Connection {
            dst_host: String::new(),
            dst_ip: ip.to_string(),
            dst_port: port,
            protocol: protocol.to_string(),
            ..Default::default()
        });
        assert_eq!(row.dst_host, ip, "the bridge's row, as built");
        row
    }

    #[test]
    fn a_connection_to_a_bare_ip_has_a_known_empty_host() {
        // The daemon's `DstHost` is empty (nothing resolved to that IP), and
        // the bridge's row then carries the IP in `dst_host`.
        let bare = ip_only("10.0.0.5", 443, "tcp");
        let form = SimulationForm::for_connection(&bare);
        assert!(form.dest_host_empty);
        assert_eq!(form.dest_host, "");
        let input = input(&bare);
        assert_eq!(input.dest_host.as_deref(), Some(""));
        assert_eq!(input.dest_ip.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn on_port_53_the_host_is_the_dns_question_so_an_ip_in_its_place_is_unknown() {
        // For tcp/udp to port 53 the daemon copies the DNS question name into
        // `DstHost` (`conman/connection.go`), so a query whose name is an IP
        // literal has `DstHost == DstIP`: indistinguishable, in the row, from
        // the bridge's stand-in for a missing host. Not known either way.
        for (ip, protocol) in [
            ("10.0.0.5", "udp"),
            ("10.0.0.5", "tcp"),
            ("2001:db8::1", "udp6"),
            ("2001:db8::1", "tcp6"),
        ] {
            let query = ip_only(ip, 53, protocol);
            let form = SimulationForm::for_connection(&query);
            assert!(!form.dest_host_empty, "{ip} {protocol}");
            assert_eq!(form.dest_host, "", "{ip} {protocol}");
            assert_eq!(input(&query).dest_host, None, "{ip} {protocol}");
        }
    }

    #[test]
    fn only_tcp_and_udp_carry_a_dns_question_so_other_protocols_on_53_are_bare() {
        for protocol in ["udplite", "udplite6", "sctp", "sctp6"] {
            let bare = ip_only("10.0.0.5", 53, protocol);
            assert_eq!(input(&bare).dest_host.as_deref(), Some(""), "{protocol}");
        }
        // A protocol the daemon doesn't name could be either: unknown.
        for protocol in ["", "gre"] {
            let odd = ip_only("10.0.0.5", 53, protocol);
            assert_eq!(input(&odd).dest_host, None, "{protocol:?}");
        }
    }

    #[test]
    fn only_port_53_makes_an_ip_in_the_host_unknown() {
        for port in [0, 52, 54, 80, 443, 5353] {
            let bare = ip_only("10.0.0.5", port, "tcp");
            assert_eq!(input(&bare).dest_host.as_deref(), Some(""), "port {port}");
        }
    }

    #[test]
    fn a_dns_question_name_is_the_host_on_port_53() {
        let query = row_of(&Connection {
            dst_host: "example.com".to_string(),
            dst_ip: "10.0.0.5".to_string(),
            dst_port: 53,
            protocol: "udp".to_string(),
            ..Default::default()
        });
        assert_eq!(input(&query).dest_host.as_deref(), Some("example.com"));
    }

    #[test]
    fn an_unknown_host_is_not_evaluated_by_a_host_rule() {
        let query = ip_only("1.1.1.1", 53, "udp");
        let store = store_of_host_rules(&[("10-ip-as-host", "simple", "1.1.1.1")]);
        let result = simulate(&store, &input(&query));
        assert_eq!(result.matched_rule, None);
        assert_eq!(result.unevaluated.len(), 1, "{:?}", result.unevaluated);
        assert_eq!(result.unevaluated[0].operand, "dest.host");
        // The same rule against a bare IP on another port: no host, no match,
        // and nothing left unknown.
        let other = ip_only("1.1.1.1", 443, "tcp");
        let result = simulate(&store, &input(&other));
        assert_eq!(result.matched_rule, None);
        assert!(result.unevaluated.is_empty());
    }

    #[test]
    fn a_bare_ip_connection_is_simulated_like_the_daemon_sees_it() {
        let bare = ip_only("10.0.0.5", 443, "tcp");
        let store = store_of_host_rules(&[
            // The IP is not the host.
            ("10-ip-is-host", "simple", "10.0.0.5"),
            // An empty host has no dot.
            ("20-no-dot", "regexp", "^[^.]*$"),
        ]);
        let result = simulate(&store, &input(&bare));
        assert_eq!(result.matched_rule.as_deref(), Some("20-no-dot"));
    }

    #[test]
    fn a_host_the_daemon_resolved_is_kept() {
        let named = input(&row());
        assert_eq!(named.dest_host.as_deref(), Some("github.com"));
        assert_eq!(named.dest_ip.as_deref(), Some("140.82.112.3"));
    }

    #[test]
    fn no_host_and_no_ip_is_the_empty_host() {
        let nothing = row_of(&Connection {
            dst_port: 53,
            ..Default::default()
        });
        assert_eq!(input(&nothing).dest_host.as_deref(), Some(""));
        assert_eq!(input(&nothing).dest_ip, None);
    }

    #[test]
    fn the_prefill_serializes_for_the_sheet_with_camel_case_keys() {
        let json = serde_json::to_value(SimulationForm::for_connection(&row())).unwrap();
        assert_eq!(json["processPath"], "/usr/bin/curl");
        assert_eq!(json["destHost"], "github.com");
        assert_eq!(json["destHostEmpty"], false);
        assert_eq!(json["destIp"], "140.82.112.3");
        assert_eq!(json["destPort"], 443);
        assert_eq!(json["protocol"], "tcp");
        // Unknowns are blank strings the sheet leaves blank, never null.
        assert_eq!(json["uid"], "");
        assert_eq!(json["checksums"], "");
    }
}
