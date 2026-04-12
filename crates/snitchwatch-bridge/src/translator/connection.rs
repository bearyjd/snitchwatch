//! Translate an opensnitchd `Connection` proto into a `ConnectionRow` for
//! the WebSocket layer.
//!
//! The `notification_id` argument is the daemon-supplied id we want to use
//! as a stable correlation handle so the WS client can later send back a
//! `setVerdict` referencing the same row.

use crate::ws_messages::ConnectionRow;
use snitchwatch_proto::protocol::Connection;

pub const ASK_ROW_PREFIX: &str = "ask-";

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
    }
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
}
