//! Translate an opensnitchd `Connection` proto into a `ConnectionRow` for
//! the WebSocket layer.
//!
//! The `notification_id` argument is the daemon-supplied id we want to use
//! as a stable correlation handle so the WS client can later send back a
//! `setVerdict` referencing the same row.

use crate::ws_messages::ConnectionRow;
use snitchwatch_proto::protocol::{Connection, Event};

pub const ASK_ROW_PREFIX: &str = "ask-";
/// Id prefix for rows synthesized from a daemon-reported `Event` (see
/// [`event_to_row`]) — connections the daemon already matched against an
/// existing rule and reports via `Statistics.events` on a `Ping` call, as
/// opposed to `ASK_ROW_PREFIX` rows the daemon is actively prompting for.
pub const EVENT_ROW_PREFIX: &str = "event-";

pub fn ask_row_id(notification_id: u64) -> String {
    format!("{ASK_ROW_PREFIX}{notification_id}")
}

pub fn connection_to_row(conn: &Connection, notification_id: u64) -> ConnectionRow {
    let process = if conn.process_path.is_empty() {
        "<unknown>".to_string()
    } else {
        std::path::Path::new(&conn.process_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string()
    };

    let process_path = if conn.process_path.is_empty() {
        None
    } else {
        Some(conn.process_path.clone())
    };

    let dst_host = if conn.dst_host.is_empty() {
        conn.dst_ip.clone()
    } else {
        conn.dst_host.clone()
    };

    ConnectionRow {
        id: ask_row_id(notification_id),
        process,
        process_path,
        dst_host,
        dst_ip: conn.dst_ip.clone(),
        dst_port: conn.dst_port as u16,
        protocol: conn.protocol.clone(),
        direction: "outgoing".to_string(),
        action: None,
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        // An AskRule row is, by construction, a connection opensnitchd found
        // no existing rule for (that's exactly why it's asking) — there is no
        // matched rule yet. `ConnectionCache::resolve` fills this in once the
        // user's verdict becomes the governing rule.
        matched_rule: None,
    }
}

/// Normalize a daemon-reported rule action string the same way
/// `snitchwatch-kirigami`'s `rules::row_store::Rule::normalized_action` does:
/// exactly `"allow"` or `"deny"`, folding anything else (opensnitchd's
/// `"reject"` included) into `"deny"`.
fn normalized_action(action: &str) -> &'static str {
    if action.eq_ignore_ascii_case("allow") {
        "allow"
    } else {
        "deny"
    }
}

/// Translate a daemon-reported `Event` (a `Connection` paired with the `Rule`
/// that decided it) into a *decided* `ConnectionRow` carrying that rule's
/// name in `matched_rule`.
///
/// The daemon includes recent `Event`s in `Statistics.events` on its
/// periodic `Ping` calls — this is how the bridge learns about connections
/// that matched a pre-existing rule and therefore never went through the
/// interactive `AskRule` flow (see `grpc_server::UiService::ping`). Returns
/// `None` when the event doesn't carry both a connection and the rule that
/// matched it — there is nothing useful to show without both.
pub fn event_to_row(event: &Event) -> Option<ConnectionRow> {
    let conn = event.connection.as_ref()?;
    let rule = event.rule.as_ref()?;

    let mut row = connection_to_row(conn, 0);
    row.id = format!("{EVENT_ROW_PREFIX}{}", event.unixnano);
    row.action = Some(normalized_action(&rule.action).to_string());
    row.matched_rule = Some(rule.name.clone());
    row.started_at_ms = event.unixnano / 1_000_000;
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            src_ip: "192.168.1.10".to_string(),
            src_port: 51544,
            dst_ip: "140.82.121.4".to_string(),
            dst_host: "github.com".to_string(),
            dst_port: 443,
            user_id: 1000,
            process_id: 4242,
            process_path: "/usr/bin/curl".to_string(),
            process_cwd: "/home/alice".to_string(),
            process_args: vec!["curl".into(), "https://github.com".into()],
            process_env: Default::default(),
            process_checksums: Default::default(),
            process_tree: vec![],
        }
    }

    #[test]
    fn ask_row_id_is_stable() {
        assert_eq!(ask_row_id(7), "ask-7");
    }

    #[test]
    fn connection_to_row_populates_all_visible_fields() {
        let conn = sample_connection();
        let row = connection_to_row(&conn, 42);

        assert_eq!(row.id, "ask-42");
        assert_eq!(row.process, "curl");
        assert_eq!(row.process_path.as_deref(), Some("/usr/bin/curl"));
        assert_eq!(row.dst_host, "github.com");
        assert_eq!(row.dst_ip, "140.82.121.4");
        assert_eq!(row.dst_port, 443);
        assert_eq!(row.protocol, "tcp");
        assert_eq!(row.direction, "outgoing");
        assert!(row.action.is_none(), "ask-rule rows start pending");
        assert_eq!(row.bytes_sent, 0);
        assert_eq!(row.bytes_received, 0);
    }

    #[test]
    fn connection_with_no_dst_host_falls_back_to_ip() {
        let mut conn = sample_connection();
        conn.dst_host = String::new();
        let row = connection_to_row(&conn, 5);
        assert_eq!(row.dst_host, "140.82.121.4");
    }

    #[test]
    fn process_basename_is_extracted_from_path() {
        let mut conn = sample_connection();
        conn.process_path = "/opt/firefox/firefox".to_string();
        let row = connection_to_row(&conn, 1);
        assert_eq!(row.process, "firefox");
    }

    #[test]
    fn process_with_empty_path_uses_unknown() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let row = connection_to_row(&conn, 1);
        assert_eq!(row.process, "<unknown>");
        assert_eq!(row.process_path, None);
    }

    #[test]
    fn ask_rows_start_with_no_matched_rule() {
        let row = connection_to_row(&sample_connection(), 1);
        assert!(row.matched_rule.is_none());
    }

    fn sample_rule(name: &str, action: &str) -> snitchwatch_proto::protocol::Rule {
        snitchwatch_proto::protocol::Rule {
            created: 1_700_000_000,
            name: name.to_string(),
            description: String::new(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: action.to_string(),
            duration: "always".to_string(),
            operator: None,
        }
    }

    #[test]
    fn event_to_row_carries_the_matched_rule_name_and_decided_action() {
        let event = Event {
            time: "2026-07-05T12:00:00Z".to_string(),
            connection: Some(sample_connection()),
            rule: Some(sample_rule("899-firefox-allow-out.json", "allow")),
            unixnano: 1_700_000_000_123_456_789,
        };
        let row = event_to_row(&event).expect("both connection and rule present");
        assert_eq!(row.id, "event-1700000000123456789");
        assert_eq!(row.process, "curl");
        assert_eq!(row.dst_host, "github.com");
        assert_eq!(row.action.as_deref(), Some("allow"));
        assert_eq!(
            row.matched_rule.as_deref(),
            Some("899-firefox-allow-out.json")
        );
        assert_eq!(row.started_at_ms, 1_700_000_000_123);
    }

    #[test]
    fn event_to_row_folds_reject_action_to_deny() {
        let event = Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: Some(sample_rule(
                "z00-blocklist:ads:0001-tracker.example",
                "reject",
            )),
            unixnano: 1,
        };
        let row = event_to_row(&event).unwrap();
        assert_eq!(row.action.as_deref(), Some("deny"));
    }

    #[test]
    fn event_to_row_is_none_without_a_connection() {
        let event = Event {
            time: String::new(),
            connection: None,
            rule: Some(sample_rule("899-firefox-allow-out.json", "allow")),
            unixnano: 1,
        };
        assert!(event_to_row(&event).is_none());
    }

    #[test]
    fn event_to_row_is_none_without_a_rule() {
        let event = Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: None,
            unixnano: 1,
        };
        assert!(event_to_row(&event).is_none());
    }
}
