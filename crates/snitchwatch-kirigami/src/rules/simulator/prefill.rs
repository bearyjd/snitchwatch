//! "Simulate this connection": filling the simulator form from a connection
//! row.
//!
//! A row carries a process path (when the bridge knows it), the destination
//! host, IP and port, and the protocol — nothing else the simulator can use.
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

/// The `DstHost` opensnitchd had for `row`'s connection.
///
/// The bridge's `connection_to_row` (`translator/connection.rs`) puts the IP
/// in `dst_host` when the daemon's `DstHost` is empty, which is what a bare-IP
/// connection has (`conman/connection.go`: `DstHost: dns.HostOr(ip, "")`). A
/// host that came from DNS is a name, never the IP literal, so `dst_host`
/// equal to `dst_ip` is that fallback and the daemon's host is the known empty
/// string. Copying it as is would simulate a host the daemon never had.
fn daemon_dst_host(row: &ConnectionRow) -> String {
    if !row.dst_ip.is_empty() && row.dst_host == row.dst_ip {
        String::new()
    } else {
        row.dst_host.clone()
    }
}

impl SimulationForm {
    /// The form for simulating `row`: its known fields set, the rest blank.
    pub fn for_connection(row: &ConnectionRow) -> Self {
        let protocol = row.protocol.trim().to_ascii_lowercase();
        Self {
            // Blank when the bridge doesn't know it; never the process name.
            process_path: row.process_path.clone().unwrap_or_default(),
            dest_host: daemon_dst_host(row),
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
            dest_host: "github.com".to_string(),
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

    #[test]
    fn a_connection_to_a_bare_ip_has_a_known_empty_host() {
        // The daemon's `DstHost` is empty (nothing resolved to that IP), and
        // the bridge's row then carries the IP in `dst_host`.
        let bare = row_of(&Connection {
            dst_host: String::new(),
            dst_ip: "10.0.0.5".to_string(),
            dst_port: 53,
            protocol: "udp".to_string(),
            ..Default::default()
        });
        assert_eq!(bare.dst_host, "10.0.0.5", "the bridge's row, as built");
        let input = input(&bare);
        assert_eq!(input.dest_host, "");
        assert_eq!(input.dest_ip.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn a_bare_ip_connection_is_simulated_like_the_daemon_sees_it() {
        let bare = row_of(&Connection {
            dst_ip: "10.0.0.5".to_string(),
            dst_port: 53,
            protocol: "udp".to_string(),
            ..Default::default()
        });
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
        assert_eq!(named.dest_host, "github.com");
        assert_eq!(named.dest_ip.as_deref(), Some("140.82.112.3"));
    }

    #[test]
    fn no_host_and_no_ip_is_the_empty_host() {
        let nothing = row_of(&Connection {
            dst_port: 53,
            ..Default::default()
        });
        assert_eq!(input(&nothing).dest_host, "");
        assert_eq!(input(&nothing).dest_ip, None);
    }

    #[test]
    fn the_prefill_serializes_for_the_sheet_with_camel_case_keys() {
        let json = serde_json::to_value(SimulationForm::for_connection(&row())).unwrap();
        assert_eq!(json["processPath"], "/usr/bin/curl");
        assert_eq!(json["destHost"], "github.com");
        assert_eq!(json["destIp"], "140.82.112.3");
        assert_eq!(json["destPort"], 443);
        assert_eq!(json["protocol"], "tcp");
        // Unknowns are blank strings the sheet leaves blank, never null.
        assert_eq!(json["uid"], "");
        assert_eq!(json["checksums"], "");
    }
}
