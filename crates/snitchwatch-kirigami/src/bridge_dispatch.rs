//! Qt-free routing + (de)serialization glue between the in-process bridge and
//! the QML models (Task 13 live wiring).
//!
//! The live feed spawns one Tokio task per receiving model, each subscribed to
//! the bridge's `broadcast::Receiver<ServerMessage>`. Every task sees *every*
//! outbound message, so each must decide whether a given message concerns its
//! model before paying to serialize + queue it onto the Qt thread. Routing a
//! message to a model whose `RowStore::apply` ignores it would still trigger a
//! needless `beginResetModel`/`endResetModel` on that model — clearing the
//! Connections view's selection/scroll on an unrelated blocklist update, for
//! instance. So routing is deliberate, not "broadcast to all and let each
//! filter internally".
//!
//! These predicates mirror exactly the variants each model's `RowStore::apply`
//! handles (see the `*::row_store` modules); they are pure functions over
//! [`ServerMessage`] and unit-tested here without any Qt dependency.

use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

/// True when `msg` mutates the connection list (drives `ConnectionsModel`).
pub fn interests_connections(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::InsertConnectionRows { .. }
            | ServerMessage::UpdateConnectionRows { .. }
            | ServerMessage::RemoveConnectionRows { .. }
            | ServerMessage::MoveConnetionRows { .. }
            | ServerMessage::ClearConnectionRows
    )
}

/// True when `msg` mutates the rule list (drives `RulesModel`).
pub fn interests_rules(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::SetRules { .. } | ServerMessage::UpdateRules { .. }
    )
}

/// True when `msg` mutates the blocklist subscription list (drives
/// `BlocklistsModel`).
pub fn interests_blocklists(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::SetBlocklists { .. }
            | ServerMessage::SetBlocklistDetails { .. }
            | ServerMessage::SetBlocklistStatus { .. }
    )
}

/// True when `msg` replaces the per-subscription entry list (drives
/// `BlocklistEntriesModel`).
pub fn interests_blocklist_entries(msg: &ServerMessage) -> bool {
    matches!(msg, ServerMessage::SetBlocklistEntries { .. })
}

/// True when `msg` carries new binned traffic samples (drives
/// `TrafficModel`). `SetTrafficData`/`UpdateTrafficData` are deliberately
/// excluded — see `crate::traffic::ring_store`'s module docs for why those
/// legacy uPlot-shaped blobs aren't consumed.
pub fn interests_traffic(msg: &ServerMessage) -> bool {
    matches!(msg, ServerMessage::TrafficEvents { .. })
}

/// Serialize an outbound `ServerMessage` to the JSON the models'
/// `applyServerMessageJson` invokable consumes.
pub fn encode_server(msg: &ServerMessage) -> Result<String, serde_json::Error> {
    serde_json::to_string(msg)
}

/// Parse an inbound client-message JSON emitted by a model request signal
/// (`verdictSubmitted` / `subscriptionRequested` / `ruleChangeRequested`) back
/// into the typed [`ClientMessage`] the bridge's inbound pump expects.
pub fn decode_client(json: &str) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::{
        BlocklistEntry, BlocklistSummary, ConnectionRow, TrafficEvent, VerdictAction, VerdictScope,
    };

    fn conn_row(id: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "firefox".into(),
            process_path: None,
            dst_host: "github.com".into(),
            dst_ip: "1.1.1.1".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }
    }

    #[test]
    fn connection_messages_route_only_to_connections() {
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![conn_row("r1")],
        };
        assert!(interests_connections(&msg));
        assert!(!interests_rules(&msg));
        assert!(!interests_blocklists(&msg));
        assert!(!interests_blocklist_entries(&msg));

        assert!(interests_connections(&ServerMessage::ClearConnectionRows));
        assert!(interests_connections(&ServerMessage::MoveConnetionRows {
            ids: vec!["r1".into()]
        }));
    }

    #[test]
    fn rule_messages_route_only_to_rules() {
        let msg = ServerMessage::SetRules { rules: vec![] };
        assert!(interests_rules(&msg));
        assert!(!interests_connections(&msg));
        assert!(!interests_blocklists(&msg));
        assert!(interests_rules(&ServerMessage::UpdateRules {
            rules: vec![]
        }));
    }

    #[test]
    fn blocklist_subscription_messages_route_only_to_blocklists() {
        let msg = ServerMessage::SetBlocklists {
            blocklists: vec![BlocklistSummary {
                id: "sb".into(),
                display_name: "StevenBlack".into(),
                url: "https://x/hosts".into(),
                entry_count: 1,
                status: "ok".into(),
                last_updated_iso8601: None,
                last_failure_reason: None,
            }],
        };
        assert!(interests_blocklists(&msg));
        assert!(!interests_blocklist_entries(&msg));
        assert!(!interests_connections(&msg));

        assert!(interests_blocklists(&ServerMessage::SetBlocklistStatus {
            subscription_id: "sb".into(),
            status: "fetching".into(),
            last_failure_reason: None,
        }));
    }

    #[test]
    fn blocklist_entry_messages_route_only_to_entries() {
        let msg = ServerMessage::SetBlocklistEntries {
            subscription_id: "sb".into(),
            entries: vec![BlocklistEntry {
                host: "doubleclick.net".into(),
            }],
        };
        assert!(interests_blocklist_entries(&msg));
        assert!(!interests_blocklists(&msg));
        assert!(!interests_connections(&msg));
    }

    #[test]
    fn traffic_event_messages_route_only_to_traffic() {
        let msg = ServerMessage::TrafficEvents {
            events: vec![TrafficEvent {
                timestamp_ms: 1_000_000_000_000,
                bytes_in: 100,
                bytes_out: 50,
            }],
        };
        assert!(interests_traffic(&msg));
        assert!(!interests_connections(&msg));
        assert!(!interests_rules(&msg));
        assert!(!interests_blocklists(&msg));
        assert!(!interests_blocklist_entries(&msg));
    }

    #[test]
    fn legacy_uplot_traffic_blobs_are_not_routed_to_traffic() {
        assert!(!interests_traffic(&ServerMessage::SetTrafficData {
            data: serde_json::json!({}),
        }));
        assert!(!interests_traffic(&ServerMessage::UpdateTrafficData {
            data: serde_json::json!({}),
        }));
    }

    #[test]
    fn encode_server_round_trips_through_apply_json_shape() {
        // The JSON the feed hands to `applyServerMessageJson` must deserialize
        // back to the identical typed message the models parse internally.
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![conn_row("r7")],
        };
        let json = encode_server(&msg).expect("encode");
        let back: ServerMessage = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, msg);
    }

    #[test]
    fn decode_client_parses_model_emitted_verdict_json() {
        // Exactly the JSON `PendingDecision::submit` emits.
        let json = r#"{"action":"setVerdict","rowId":"r1","verdict":"deny","scope":"any_host","remember":true}"#;
        match decode_client(json).expect("decode") {
            ClientMessage::SetVerdict {
                row_id,
                verdict,
                scope,
                remember,
            } => {
                assert_eq!(row_id, "r1");
                assert_eq!(verdict, VerdictAction::Deny);
                assert_eq!(scope, VerdictScope::AnyHost);
                assert!(remember);
            }
            other => panic!("expected SetVerdict, got {other:?}"),
        }
    }

    #[test]
    fn decode_client_parses_subscribe_and_rule_change_json() {
        match decode_client(r#"{"action":"subscribeBlocklist","url":"https://x/h"}"#).unwrap() {
            ClientMessage::SubscribeBlocklist { url } => assert_eq!(url, "https://x/h"),
            other => panic!("expected SubscribeBlocklist, got {other:?}"),
        }
        match decode_client(r#"{"action":"deleteRule","ruleId":"block-ads"}"#).unwrap() {
            ClientMessage::DeleteRule { rule_id } => assert_eq!(rule_id, "block-ads"),
            other => panic!("expected DeleteRule, got {other:?}"),
        }
    }

    #[test]
    fn decode_client_rejects_malformed_json() {
        assert!(decode_client("{ not json").is_err());
        assert!(decode_client(r#"{"action":"noSuchAction"}"#).is_err());
    }
}
