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

/// True when `msg` mutates the connection list — the same variants
/// `interests_connections` cares about, but drives `GeoModel`'s per-country
/// aggregate instead. Kept as its own named predicate (rather than reusing
/// `interests_connections` directly at the call site) so each model's feed
/// task reads as "this predicate is *my* routing rule", matching the style of
/// every other `interests_*` function here — even though the two currently
/// happen to match an identical variant set, they're conceptually different
/// consumers (an ordered row list vs. a country-keyed aggregate) and may
/// diverge later (e.g. if the geo panel starts consuming a dedicated
/// traffic-volume message the row list doesn't need).
pub fn interests_geo(msg: &ServerMessage) -> bool {
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

/// True when `msg` mutates the profile list or active-profile selection
/// (drives `ProfilesModel`).
pub fn interests_profiles(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::SetProfiles { .. } | ServerMessage::ProfileChanged { .. }
    )
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

/// The shared feed loop every model's `startBridgeFeed()` runs (Qt-free, so
/// it's directly unit-testable below). Receives every outbound
/// [`ServerMessage`], keeps only those `interest` routes to this model,
/// encodes to the JSON `applyServerMessageJson` consumes, and hands
/// `(typed message, json)` to `deliver` — the model-supplied closure that
/// queues onto the Qt thread (and, for `GeoModel`, warms the GeoIP cache
/// first; that's why the typed message is passed alongside the JSON).
///
/// On [`broadcast::error::RecvError::Lagged`] the loop asks the bridge to
/// re-broadcast full snapshots ([`ClientMessage::RequestSnapshot`]): these are
/// stateful snapshot+delta streams, so silently skipping deltas would leave
/// the model stale until the next natural snapshot — which for connections
/// never comes. Exits when either channel closes.
pub async fn run_feed<F>(
    mut rx: tokio::sync::broadcast::Receiver<ServerMessage>,
    inbound: tokio::sync::mpsc::Sender<ClientMessage>,
    label: &'static str,
    interest: fn(&ServerMessage) -> bool,
    deliver: F,
) where
    F: Fn(&ServerMessage, String) + Send + 'static,
{
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(msg) => {
                if !interest(&msg) {
                    continue;
                }
                match encode_server(&msg) {
                    Ok(json) => deliver(&msg, json),
                    Err(e) => tracing::warn!(feed = label, error = %e, "feed: encode failed"),
                }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(
                    feed = label,
                    skipped = n,
                    "feed lagged behind bridge; requesting snapshot resync"
                );
                if inbound.send(ClientMessage::RequestSnapshot).await.is_err() {
                    break;
                }
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// Spawn [`run_feed`] for one model on the bridge runtime. This is the whole
/// body of every model's `startBridgeFeed()` beyond obtaining its
/// `CxxQtThread` handle — one place to change feed behavior for all models.
pub fn spawn_feed<F>(
    handles: &crate::bridge_runtime::BridgeHandles,
    label: &'static str,
    interest: fn(&ServerMessage) -> bool,
    deliver: F,
) where
    F: Fn(&ServerMessage, String) + Send + 'static,
{
    let rx = handles.subscribe();
    let inbound = handles.inbound_tx();
    handles
        .runtime()
        .spawn(run_feed(rx, inbound, label, interest, deliver));
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::{
        BlocklistEntry, BlocklistSummary, ConnectionRow, TrafficEvent, VerdictAction,
        VerdictDuration, VerdictScope,
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
            matched_rule: None,
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
    fn connection_messages_also_route_to_geo() {
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![conn_row("r1")],
        };
        assert!(interests_geo(&msg));
        assert!(interests_geo(&ServerMessage::UpdateConnectionRows {
            rows: vec![conn_row("r1")]
        }));
        assert!(interests_geo(&ServerMessage::RemoveConnectionRows {
            ids: vec!["r1".into()]
        }));
        assert!(interests_geo(&ServerMessage::MoveConnetionRows {
            ids: vec!["r1".into()]
        }));
        assert!(interests_geo(&ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn unrelated_messages_do_not_route_to_geo() {
        assert!(!interests_geo(&ServerMessage::SetRules { rules: vec![] }));
        assert!(!interests_geo(&ServerMessage::SetConnectionsStatus {
            status: snitchwatch_bridge::ws_messages::ConnectionsStatus::Connected
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
    fn profile_messages_route_only_to_profiles() {
        use snitchwatch_bridge::ws_messages::{ProfileRuleWire, ProfileSummary};

        let msg = ServerMessage::SetProfiles {
            profiles: vec![ProfileSummary {
                id: "home".into(),
                name: "At Home".into(),
                network_matchers: vec!["Home*".into()],
                rules: vec![ProfileRuleWire {
                    id: "r1".into(),
                    action: "allow".into(),
                    operand: "dest.host".into(),
                    data: "nas.local".into(),
                }],
                active: true,
            }],
        };
        assert!(interests_profiles(&msg));
        assert!(!interests_connections(&msg));
        assert!(!interests_rules(&msg));
        assert!(!interests_blocklists(&msg));
        assert!(!interests_blocklist_entries(&msg));
        assert!(!interests_traffic(&msg));

        assert!(interests_profiles(&ServerMessage::ProfileChanged {
            active_profile_id: Some("home".into())
        }));
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
        let json = r#"{"action":"setVerdict","rowId":"r1","verdict":"deny","scope":"any_host","duration":"always"}"#;
        match decode_client(json).expect("decode") {
            ClientMessage::SetVerdict {
                row_id,
                verdict,
                scope,
                duration,
                remember,
            } => {
                assert_eq!(row_id, "r1");
                assert_eq!(verdict, VerdictAction::Deny);
                assert_eq!(scope, VerdictScope::AnyHost);
                assert_eq!(duration, Some(VerdictDuration::Always));
                assert_eq!(remember, None);
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
    fn decode_client_parses_profile_json() {
        match decode_client(
            r#"{"action":"createProfile","id":"home","name":"At Home","networkMatchers":["Home*"]}"#,
        )
        .unwrap()
        {
            ClientMessage::CreateProfile {
                id,
                name,
                network_matchers,
            } => {
                assert_eq!(id, "home");
                assert_eq!(name, "At Home");
                assert_eq!(network_matchers, vec!["Home*".to_string()]);
            }
            other => panic!("expected CreateProfile, got {other:?}"),
        }
        match decode_client(r#"{"action":"activateProfile","id":"home"}"#).unwrap() {
            ClientMessage::ActivateProfile { id } => assert_eq!(id, "home"),
            other => panic!("expected ActivateProfile, got {other:?}"),
        }
        match decode_client(r#"{"action":"deactivateProfile"}"#).unwrap() {
            ClientMessage::DeactivateProfile => {}
            other => panic!("expected DeactivateProfile, got {other:?}"),
        }
    }

    #[test]
    fn decode_client_rejects_malformed_json() {
        assert!(decode_client("{ not json").is_err());
        assert!(decode_client(r#"{"action":"noSuchAction"}"#).is_err());
    }

    #[tokio::test]
    async fn run_feed_delivers_only_messages_its_predicate_routes() {
        let (btx, brx) = tokio::sync::broadcast::channel(16);
        let (itx, _irx) = tokio::sync::mpsc::channel(4);
        let (dtx, mut drx) = tokio::sync::mpsc::unbounded_channel();

        let feed = tokio::spawn(run_feed(
            brx,
            itx,
            "test-rules",
            interests_rules,
            move |msg, json| {
                assert!(
                    interests_rules(msg),
                    "deliver must only see routed messages"
                );
                let _ = dtx.send(json);
            },
        ));

        btx.send(ServerMessage::SetRules { rules: vec![] }).unwrap();
        btx.send(ServerMessage::ClearConnectionRows).unwrap(); // filtered out
        btx.send(ServerMessage::UpdateRules { rules: vec![] })
            .unwrap();

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), drx.recv())
            .await
            .expect("no delivery")
            .expect("deliver channel closed");
        assert!(
            first.contains("etRules"),
            "expected a rules message, got {first}"
        );
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), drx.recv())
            .await
            .expect("no second delivery")
            .expect("deliver channel closed");
        assert!(
            second.contains("Rules"),
            "expected a rules message, got {second}"
        );

        drop(btx); // Closed → loop exits
        tokio::time::timeout(std::time::Duration::from_secs(1), feed)
            .await
            .expect("feed did not exit on channel close")
            .unwrap();
    }

    #[tokio::test]
    async fn run_feed_requests_snapshot_resync_after_lagging() {
        // Capacity-1 broadcast: overrunning it before the consumer task starts
        // guarantees the first recv() observes Lagged.
        let (btx, brx) = tokio::sync::broadcast::channel(1);
        let (itx, mut irx) = tokio::sync::mpsc::channel(4);

        btx.send(ServerMessage::ClearConnectionRows).unwrap();
        btx.send(ServerMessage::ClearConnectionRows).unwrap();
        btx.send(ServerMessage::ClearConnectionRows).unwrap();

        tokio::spawn(run_feed(
            brx,
            itx,
            "test-lag",
            interests_connections,
            |_msg, _json| {},
        ));

        let resync = tokio::time::timeout(std::time::Duration::from_secs(1), irx.recv())
            .await
            .expect("no snapshot request after lag")
            .expect("inbound channel closed");
        assert_eq!(resync, ClientMessage::RequestSnapshot);
    }
}
