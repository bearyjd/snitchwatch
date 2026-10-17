//! Qt-free routing + (de)serialization glue between the external bridge
//! service client and the QML models (Task 13 live wiring).
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

use crate::bridge_runtime::{ReceivedServerMessage, SendClientMessageError};
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

/// True when `msg` drives `ConnectionsModel`: the connection list, plus the
/// rule list and rule command results its tree's rule buttons show
/// (`connections::level_rules`).
pub fn interests_monitor(msg: &ServerMessage) -> bool {
    interests_connections(msg)
        || matches!(
            msg,
            ServerMessage::SetRules { .. }
                | ServerMessage::UpdateRules { .. }
                | ServerMessage::RuleCommandResult { .. }
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

/// True when `msg` mutates the rule list or carries the rules' hit counts
/// (drives `RulesModel`).
pub fn interests_rules(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::SetRules { .. }
            | ServerMessage::UpdateRules { .. }
            | ServerMessage::RuleHits { .. }
            | ServerMessage::RulesNotShown { .. }
    )
}

/// True when `msg` mutates the blocklist subscription list (drives
/// `BlocklistsModel`).
pub fn interests_blocklists(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::SetBlocklists { .. }
            | ServerMessage::SetBlocklistLeftovers { .. }
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
        ServerMessage::SetProfiles { .. }
            | ServerMessage::ProfileChanged { .. }
            | ServerMessage::RuleCommandResult { .. }
    )
}

/// True when `msg` carries new binned traffic samples or daemon aggregate
/// statistics (drives `TrafficModel`, issue #19). `SetTrafficData`/
/// `UpdateTrafficData` are deliberately excluded — see
/// `crate::traffic::ring_store`'s module docs for why those legacy uPlot-
/// shaped blobs aren't consumed.
pub fn interests_traffic(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::TrafficEvents { .. } | ServerMessage::DaemonStatistics { .. }
    )
}

/// True when `msg` carries the recommended background-service rules, or
/// starts a session's snapshot (so `CuratedDefaultsModel`, prompt-slot D,
/// forgets an earlier bridge's list even when the new one sends none).
pub fn interests_curated_defaults(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::SetCuratedDefaults { .. }
            | ServerMessage::ClearConnectionRows
            | ServerMessage::Authenticated { .. }
            | ServerMessage::CuratedBroadOptInPrepared { .. }
            | ServerMessage::CuratedDefaultsRequestResult { .. }
    )
}

/// True when `msg` carries daemon diagnostics report data (drives
/// `DaemonHealthModel`).
pub fn interests_diagnostics(msg: &ServerMessage) -> bool {
    matches!(msg, ServerMessage::DiagnosticsReport { .. })
}

/// True when `msg` reports that a `Deny` verdict's requested scope was
/// silently narrowed (issue #14 security review round 2, HIGH — see
/// `ServerMessage::DenyScopeNarrowed`'s doc comment). No dedicated QML
/// model/toast consumes this yet — this predicate exists for parity with
/// every other `ServerMessage` variant having a named `interests_*` route
/// here, so a future in-app banner feature has the routing decision
/// already made rather than needing to rediscover the convention.
pub fn interests_deny_scope_narrowed(msg: &ServerMessage) -> bool {
    matches!(msg, ServerMessage::DenyScopeNarrowed { .. })
}

/// Serialize an outbound `ServerMessage` to the JSON the models'
/// `applyServerMessageJson` invokable consumes.
pub fn encode_server(msg: &ServerMessage) -> Result<String, serde_json::Error> {
    serde_json::to_string(msg)
}

/// Parse an inbound client-message JSON emitted by a model request signal
/// (`subscriptionRequested` / `ruleChangeRequested`) back into the typed
/// [`ClientMessage`] the bridge's inbound pump expects.
///
/// Verdicts do *not* come through here: `BridgeFeed::submitVerdict` builds a
/// typed message via [`crate::pending_decision`] and dispatches it directly,
/// so an allow/deny never round-trips through JSON.
pub fn decode_client(json: &str) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_str(json)
}

/// The shared feed loop every model's `startBridgeFeed()` runs, and the
/// rule-command controllers' result feeds (`crate::result_feed`) too
/// (Qt-free, so it's directly unit-testable below). Receives every outbound
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
/// never comes. A snapshot is also requested after subscription. A result
/// feed's `request_snapshot` asks for nothing: results are no state. Transient
/// disconnects or a full request queue keep the feed alive and retry the
/// snapshot until accepted. Exits when the broadcast closes or the client
/// runtime has stopped.
pub async fn run_feed<F, R>(
    mut rx: tokio::sync::broadcast::Receiver<ReceivedServerMessage>,
    request_snapshot: R,
    label: &'static str,
    interest: fn(&ServerMessage) -> bool,
    deliver: F,
) where
    F: Fn(u64, &ServerMessage, String) + Send + 'static,
    R: Fn() -> Result<(), SendClientMessageError> + Send + 'static,
{
    use tokio::sync::broadcast::error::RecvError;
    let mut resync_pending = true;
    let mut retry = tokio::time::interval(std::time::Duration::from_millis(100));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = retry.tick(), if resync_pending => {
                match request_snapshot() {
                    Ok(()) => resync_pending = false,
                    Err(
                        SendClientMessageError::Disconnected
                        | SendClientMessageError::Full
                        | SendClientMessageError::StaleSession,
                    ) => {}
                    Err(SendClientMessageError::Stopped) => break,
                }
            }
            received = rx.recv() => match received {
            Ok(received) => {
                if !interest(&received.message) {
                    continue;
                }
                match encode_server(&received.message) {
                    Ok(json) => deliver(received.connection_id, &received.message, json),
                    Err(e) => tracing::warn!(feed = label, error = %e, "feed: encode failed"),
                }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(
                    feed = label,
                    skipped = n,
                    "feed lagged behind bridge; requesting snapshot resync"
                );
                resync_pending = true;
                retry.reset_immediately();
            }
            Err(RecvError::Closed) => break,
        }
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
    F: Fn(u64, &ServerMessage, String) + Send + 'static,
{
    let rx = handles.subscribe();
    // Subscribe before asking for a snapshot. The client runtime also requests
    // one on WebSocket connect, but that early broadcast can precede QML feed
    // creation. Each state feed therefore requests a resync; the last QML feed
    // to subscribe also covers every feed created before it. (The result
    // feeds, `crate::result_feed`, request none.)
    handles.runtime().spawn(run_feed(
        rx,
        {
            let handles = handles.clone();
            move || handles.try_send(ClientMessage::RequestSnapshot)
        },
        label,
        interest,
        move |connection_id, message, json| deliver(connection_id, message, json),
    ));
}

#[cfg(test)]
mod tests;
