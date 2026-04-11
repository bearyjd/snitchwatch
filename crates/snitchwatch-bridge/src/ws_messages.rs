//! Serde structs for the 22 LS WebSocket message types.
//!
//! All server-to-client messages share the same envelope: `{action: "...", ...}`.
//! We model this as a tagged enum for round-trip type safety.
//!
//! TODO(task-7-followup): capture real `setVerdict` payloads from the local LS
//! instance at `http://localhost:3031/` (browser devtools → WS → messages) and
//! reconcile the `verdict` field name with what LS actually sends.

use serde::{Deserialize, Serialize};

/// Server → client message envelope. Each variant matches one of the 22
/// `handleServerCommand` cases in the LS UI's `app.js`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    InsertConnectionRows { rows: Vec<ConnectionRow> },
    UpdateConnectionRows { rows: Vec<ConnectionRow> },
    RemoveConnectionRows { ids: Vec<String> },
    /// Note the typo: this is in upstream LS, we preserve it.
    #[serde(rename = "moveConnetionRows")]
    MoveConnetionRows { ids: Vec<String> },
    ClearConnectionRows,
    SetInspector { inspector: serde_json::Value },
    UpdateRuleButtons { buttons: serde_json::Value },
    HighlightRuleForRows { rule_id: String, row_ids: Vec<String> },
    TrafficEvents { events: Vec<TrafficEvent> },
    SetTrafficData { data: serde_json::Value },
    UpdateTrafficData { data: serde_json::Value },
    SetRules { rules: Vec<serde_json::Value> },
    UpdateRules { rules: Vec<serde_json::Value> },
    SetBlocklists { blocklists: Vec<serde_json::Value> },
    SetBlocklistDetails { details: serde_json::Value },
    SetBlocklistEntries { entries: Vec<serde_json::Value> },
    SetBlocklistEntryLocation { location: serde_json::Value },
    SetBlocklistStatus { status: serde_json::Value },
    SetConnectionsStatus { status: ConnectionsStatus },
    SetAboutInfo { info: AboutInfo },
    SetUndoStack { stack: Vec<serde_json::Value> },
    LocalizationTable { table: serde_json::Value },
    GlobalSettings { settings: serde_json::Value },
}

/// Client → server messages. These come from the UI's `sendAction(type, payload)`
/// calls. The `action` discriminator is the type name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    SetVerdict {
        row_id: String,
        /// "allow" or "deny" — named `verdict` to avoid colliding with the
        /// envelope's `action` discriminator. Verify against captured LS payload.
        verdict: VerdictAction,
        scope: VerdictScope,
        remember: bool,
    },
    AddRule {
        rule: serde_json::Value,
    },
    UpdateRule {
        rule_id: String,
        rule: serde_json::Value,
    },
    DeleteRule {
        rule_id: String,
    },
    GlobalSettings {
        settings: serde_json::Value,
    },
    SubscribeBlocklist {
        url: String,
    },
    UnsubscribeBlocklist {
        id: String,
    },
    Undo,
    Redo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VerdictAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictScope {
    /// Exact destination host only.
    ThisHost,
    /// Wildcard the leftmost label of the destination host.
    AnyHostOnDomain,
    /// Drop the host operator entirely.
    AnyHost,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRow {
    pub id: String,
    pub process: String,
    pub process_path: Option<String>,
    pub dst_host: String,
    pub dst_ip: String,
    pub dst_port: u16,
    pub protocol: String,
    pub direction: String,
    /// `null` for pending rows, `"allow"` / `"deny"` / `"blocklist"` once decided.
    pub action: Option<String>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficEvent {
    pub timestamp_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionsStatus {
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AboutInfo {
    pub snitchwatch_version: String,
    pub opensnitchd_version: String,
    pub ebpf_commit: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_message_round_trips_via_json() {
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![ConnectionRow {
                id: "r1".to_string(),
                process: "firefox".to_string(),
                process_path: Some("/usr/bin/firefox".to_string()),
                dst_host: "github.com".to_string(),
                dst_ip: "140.82.121.4".to_string(),
                dst_port: 443,
                protocol: "tcp".to_string(),
                direction: "outgoing".to_string(),
                action: None,
                bytes_sent: 0,
                bytes_received: 0,
                started_at_ms: 1_700_000_000_000,
            }],
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""action":"insertConnectionRows""#));
        assert!(json.contains(r#""dstHost":"github.com""#));

        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn move_connection_rows_preserves_upstream_typo() {
        let msg = ServerMessage::MoveConnetionRows {
            ids: vec!["r1".to_string()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains(r#""action":"moveConnetionRows""#),
            "must preserve upstream LS typo: {}",
            json
        );
    }

    #[test]
    fn client_set_verdict_parses() {
        let json = r#"{
            "action": "setVerdict",
            "rowId": "r1",
            "verdict": "allow",
            "scope": "this_host",
            "remember": true
        }"#;
        let parsed: ClientMessage = serde_json::from_str(json).unwrap();
        match parsed {
            ClientMessage::SetVerdict {
                row_id,
                verdict,
                scope,
                remember,
            } => {
                assert_eq!(row_id, "r1");
                assert_eq!(verdict, VerdictAction::Allow);
                assert_eq!(scope, VerdictScope::ThisHost);
                assert!(remember);
            }
            _ => panic!("wrong variant"),
        }
    }
}
