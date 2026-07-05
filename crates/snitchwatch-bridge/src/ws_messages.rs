//! Serde structs for the 22 LS WebSocket message types.
//!
//! All server-to-client messages share the same envelope: `{action: "...", ...}`.
//! We model this as a tagged enum for round-trip type safety.

use serde::{Deserialize, Serialize};

/// Server → client message envelope. Each variant matches one of the 22
/// `handleServerCommand` cases in the LS UI's `app.js`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    InsertConnectionRows {
        rows: Vec<ConnectionRow>,
    },
    UpdateConnectionRows {
        rows: Vec<ConnectionRow>,
    },
    RemoveConnectionRows {
        ids: Vec<String>,
    },
    /// Note the typo: this is in upstream LS, we preserve it.
    #[serde(rename = "moveConnetionRows")]
    MoveConnetionRows {
        ids: Vec<String>,
    },
    ClearConnectionRows,
    SetInspector {
        inspector: serde_json::Value,
    },
    UpdateRuleButtons {
        buttons: serde_json::Value,
    },
    HighlightRuleForRows {
        rule_id: String,
        row_ids: Vec<String>,
    },
    TrafficEvents {
        events: Vec<TrafficEvent>,
    },
    SetTrafficData {
        data: serde_json::Value,
    },
    UpdateTrafficData {
        data: serde_json::Value,
    },
    SetRules {
        rules: Vec<serde_json::Value>,
    },
    UpdateRules {
        rules: Vec<serde_json::Value>,
    },
    SetBlocklists {
        blocklists: Vec<BlocklistSummary>,
    },
    SetBlocklistDetails {
        details: BlocklistSummary,
    },
    SetBlocklistEntries {
        subscription_id: String,
        entries: Vec<BlocklistEntry>,
    },
    SetBlocklistEntryLocation {
        subscription_id: String,
        host: String,
        line_number: u64,
    },
    SetBlocklistStatus {
        subscription_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_failure_reason: Option<String>,
    },
    SetProfiles {
        profiles: Vec<ProfileSummary>,
    },
    ProfileChanged {
        active_profile_id: Option<String>,
    },
    SetConnectionsStatus {
        status: ConnectionsStatus,
    },
    SetAboutInfo {
        info: AboutInfo,
    },
    SetUndoStack {
        stack: Vec<serde_json::Value>,
    },
    LocalizationTable {
        table: serde_json::Value,
    },
    GlobalSettings {
        settings: serde_json::Value,
    },
}

/// Client → server messages. These come from the UI's `sendAction(type, payload)`
/// calls. The `action` discriminator is the type name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
    CreateProfile {
        id: String,
        name: String,
        network_matchers: Vec<String>,
    },
    UpdateProfile {
        id: String,
        name: String,
        network_matchers: Vec<String>,
    },
    DeleteProfile {
        id: String,
    },
    ActivateProfile {
        id: String,
    },
    DeactivateProfile,
    AddProfileRule {
        profile_id: String,
        rule: ProfileRuleWire,
    },
    RemoveProfileRule {
        profile_id: String,
        rule_id: String,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistSummary {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub entry_count: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_iso8601: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistEntry {
    pub host: String,
}

/// One rule override within a profile, as carried over the wire (mirrors
/// `snitchwatch_bridge::profiles::store::ProfileRule`, kept as a distinct
/// type here the same way `BlocklistSummary` is distinct from
/// `blocklists::store::Subscription` — the wire shape is the stable
/// contract, the store shape is free to evolve independently).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRuleWire {
    pub id: String,
    pub action: String,
    pub operand: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub network_matchers: Vec<String>,
    pub rules: Vec<ProfileRuleWire>,
    pub active: bool,
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

#[cfg(test)]
mod blocklist_message_tests {
    use super::*;

    #[test]
    fn set_blocklists_serializes_to_camel_case_action() {
        let msg = ServerMessage::SetBlocklists {
            blocklists: vec![BlocklistSummary {
                id: "stevenblack".into(),
                display_name: "StevenBlack".into(),
                url: "https://x.example/hosts".into(),
                entry_count: 1234,
                status: "ok".into(),
                last_updated_iso8601: Some("2026-04-11T12:00:00Z".into()),
                last_failure_reason: None,
            }],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklists");
        assert_eq!(json["blocklists"][0]["id"], "stevenblack");
        assert_eq!(json["blocklists"][0]["displayName"], "StevenBlack");
        assert_eq!(json["blocklists"][0]["entryCount"], 1234);
    }

    #[test]
    fn set_blocklist_entries_carries_strongly_typed_entries() {
        let msg = ServerMessage::SetBlocklistEntries {
            subscription_id: "stevenblack".into(),
            entries: vec![
                BlocklistEntry {
                    host: "doubleclick.net".into(),
                },
                BlocklistEntry {
                    host: "google-analytics.com".into(),
                },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklistEntries");
        assert_eq!(json["subscriptionId"], "stevenblack");
        assert_eq!(json["entries"][0]["host"], "doubleclick.net");
    }

    #[test]
    fn subscribe_blocklist_action_round_trips() {
        let action = ClientMessage::SubscribeBlocklist {
            url: "https://x.example/hosts".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }
}

#[cfg(test)]
mod profile_message_tests {
    use super::*;

    fn summary(id: &str, active: bool) -> ProfileSummary {
        ProfileSummary {
            id: id.into(),
            name: "At Home".into(),
            network_matchers: vec!["Home*".into()],
            rules: vec![ProfileRuleWire {
                id: "r1".into(),
                action: "allow".into(),
                operand: "dest.host".into(),
                data: "nas.local".into(),
            }],
            active,
        }
    }

    #[test]
    fn set_profiles_serializes_to_camel_case_action() {
        let msg = ServerMessage::SetProfiles {
            profiles: vec![summary("home", true)],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setProfiles");
        assert_eq!(json["profiles"][0]["id"], "home");
        assert_eq!(json["profiles"][0]["networkMatchers"][0], "Home*");
        assert_eq!(json["profiles"][0]["rules"][0]["operand"], "dest.host");
        assert_eq!(json["profiles"][0]["active"], true);
    }

    #[test]
    fn set_profiles_round_trips() {
        let msg = ServerMessage::SetProfiles {
            profiles: vec![summary("home", false)],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn profile_changed_round_trips_with_and_without_active_id() {
        let with_id = ServerMessage::ProfileChanged {
            active_profile_id: Some("home".into()),
        };
        let json = serde_json::to_string(&with_id).unwrap();
        assert!(json.contains(r#""action":"profileChanged""#));
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            with_id
        );

        let none = ServerMessage::ProfileChanged {
            active_profile_id: None,
        };
        let json = serde_json::to_string(&none).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&json).unwrap(), none);
    }

    #[test]
    fn create_profile_round_trips() {
        let action = ClientMessage::CreateProfile {
            id: "home".into(),
            name: "At Home".into(),
            network_matchers: vec!["Home*".into()],
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains(r#""action":"createProfile""#));
        assert!(json.contains(r#""networkMatchers""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            action
        );
    }

    #[test]
    fn activate_and_deactivate_profile_round_trip() {
        let activate = ClientMessage::ActivateProfile { id: "home".into() };
        let json = serde_json::to_string(&activate).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            activate
        );

        let deactivate = ClientMessage::DeactivateProfile;
        let json = serde_json::to_string(&deactivate).unwrap();
        assert_eq!(json, r#"{"action":"deactivateProfile"}"#);
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            deactivate
        );
    }

    #[test]
    fn add_and_remove_profile_rule_round_trip() {
        let add = ClientMessage::AddProfileRule {
            profile_id: "home".into(),
            rule: ProfileRuleWire {
                id: "r1".into(),
                action: "deny".into(),
                operand: "dest.host".into(),
                data: "ads.example".into(),
            },
        };
        let json = serde_json::to_string(&add).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), add);

        let remove = ClientMessage::RemoveProfileRule {
            profile_id: "home".into(),
            rule_id: "r1".into(),
        };
        let json = serde_json::to_string(&remove).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            remove
        );
    }
}
