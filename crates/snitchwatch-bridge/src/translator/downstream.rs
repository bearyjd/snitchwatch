//! Translate opensnitchd Notification events into LS WebSocket ServerMessages.
//!
//! Plan 1 / Task 20 transport note
//! ---------------------------------
//! The real opensnitchd `Action` enum has **no** `ASK_RULE` variant. In the
//! real architecture, AskRule is a separate unary RPC where the daemon is the
//! gRPC client and the GUI is the server (see `docs/m0-spike-findings.md`).
//!
//! For M1 we keep the simpler opposite topology (bridge dials mock-as-server)
//! and piggyback ask-rule events onto the `Notifications` bidi stream. To do
//! that we pack them into `Notification.data` as a JSON envelope:
//!
//! ```text
//! { "kind": "ask_rule",
//!   "id": 42,
//!   "process": "curl",
//!   "host": "github.com",
//!   "ip": "140.82.121.4",
//!   "port": 443,
//!   "protocol": "tcp" }
//! ```
//!
//! This is **testing-only**. When M2 flips the topology, the proper AskRule
//! unary RPC replaces this envelope and the envelope code gets deleted.

use crate::ws_messages::ConnectionRow;
use serde::Deserialize;
use snitchwatch_proto::protocol::Notification;

/// Outcome of translating one Notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Translated {
    /// This notification was an AskRule and the bridge should call
    /// `cache.insert_pending` with the produced row, then await the
    /// resulting oneshot before replying.
    ///
    /// Boxed to keep the enum small (see clippy::large_enum_variant).
    AskRule(Box<ConnectionRow>),
    /// Notification not relevant to the UI; ignore.
    Ignored,
}

/// JSON envelope we expect inside `Notification.data` for ask-rule events.
#[derive(Debug, Deserialize)]
struct AskRuleEnvelope {
    kind: String,
    id: u64,
    process: String,
    #[serde(default)]
    process_path: Option<String>,
    host: String,
    ip: String,
    port: u16,
    protocol: String,
}

/// Stable row-id prefix so WS clients can tell ask-rule rows apart from
/// decided-stat rows at a glance.
const ASK_ROW_PREFIX: &str = "ask-";

/// Build the row-id for a given ask-rule notification. Exposed so callers
/// (e.g. the round-trip pump in `bridge-cli`) can correlate the WS row back
/// to the originating notification id.
pub fn ask_row_id(notification_id: u64) -> String {
    format!("{ASK_ROW_PREFIX}{notification_id}")
}

/// Translate one Notification. Until the M2 topology flip, we only recognize
/// ask-rule envelopes in `Notification.data`; everything else is ignored.
pub fn translate_notification(n: &Notification) -> Translated {
    if n.data.is_empty() {
        return Translated::Ignored;
    }
    let env: AskRuleEnvelope = match serde_json::from_str(&n.data) {
        Ok(e) => e,
        Err(_) => return Translated::Ignored,
    };
    if env.kind != "ask_rule" {
        return Translated::Ignored;
    }

    let row = ConnectionRow {
        id: ask_row_id(env.id),
        process: env.process,
        process_path: env.process_path,
        dst_host: env.host,
        dst_ip: env.ip,
        dst_port: env.port,
        protocol: env.protocol,
        direction: "outgoing".to_string(),
        action: None, // pending until the user decides
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
    };
    Translated::AskRule(Box::new(row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask_notification(id: u64) -> Notification {
        let data = json!({
            "kind": "ask_rule",
            "id": id,
            "process": "curl",
            "process_path": "/usr/bin/curl",
            "host": "github.com",
            "ip": "140.82.121.4",
            "port": 443,
            "protocol": "tcp",
        })
        .to_string();
        Notification {
            id,
            data,
            ..Default::default()
        }
    }

    #[test]
    fn default_notification_is_ignored() {
        let n = Notification::default();
        assert_eq!(translate_notification(&n), Translated::Ignored);
    }

    #[test]
    fn notification_with_unknown_data_is_ignored() {
        let n = Notification {
            data: r#"{"kind":"change_config","timeout":30}"#.to_string(),
            ..Default::default()
        };
        assert_eq!(translate_notification(&n), Translated::Ignored);
    }

    #[test]
    fn notification_with_garbage_data_is_ignored() {
        let n = Notification {
            data: "not json at all".to_string(),
            ..Default::default()
        };
        assert_eq!(translate_notification(&n), Translated::Ignored);
    }

    #[test]
    fn ask_rule_envelope_produces_pending_row() {
        let n = ask_notification(42);
        match translate_notification(&n) {
            Translated::AskRule(boxed) => {
                let row = *boxed;
                assert_eq!(row.id, "ask-42");
                assert_eq!(row.process, "curl");
                assert_eq!(row.process_path.as_deref(), Some("/usr/bin/curl"));
                assert_eq!(row.dst_host, "github.com");
                assert_eq!(row.dst_ip, "140.82.121.4");
                assert_eq!(row.dst_port, 443);
                assert_eq!(row.protocol, "tcp");
                assert_eq!(row.direction, "outgoing");
                assert!(row.action.is_none(), "ask-rule rows start pending");
            }
            other => panic!("expected AskRule, got {other:?}"),
        }
    }

    #[test]
    fn ask_row_id_is_stable() {
        assert_eq!(ask_row_id(7), "ask-7");
    }
}
