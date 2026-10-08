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

impl SimulationForm {
    /// The form for simulating `row`: its known fields set, the rest blank.
    pub fn for_connection(row: &ConnectionRow) -> Self {
        let protocol = row.protocol.trim().to_ascii_lowercase();
        Self {
            // Blank when the bridge doesn't know it; never the process name.
            process_path: row.process_path.clone().unwrap_or_default(),
            dest_host: row.dst_host.clone(),
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
    use crate::rules::simulator::SimulationInput;

    fn row() -> ConnectionRow {
        ConnectionRow {
            id: "r1".to_string(),
            process: "curl".to_string(),
            process_path: Some("/usr/bin/curl".to_string()),
            dst_host: "github.com".to_string(),
            dst_ip: "140.82.112.3".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: Some("allow".to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
        }
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
        // The daemon's DstHost is "" for a bare IP, and the row says so.
        let bare = ConnectionRow {
            dst_host: String::new(),
            ..row()
        };
        assert_eq!(input(&bare).dest_host, "");
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
