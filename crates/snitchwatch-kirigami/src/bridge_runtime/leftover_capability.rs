//! What the client does about a bridge that doesn't report leftover blocklist
//! rules (issue #73).

use tokio::sync::broadcast;

use super::ReceivedServerMessage;
use snitchwatch_bridge::ws_messages::ServerMessage;

/// A bridge that doesn't advertise `blocklistLeftovers` never reports "none
/// left", so the count an earlier bridge sent (the model is kept across
/// reconnects) would stay on the page for ever: say none are known.
pub(super) fn clear_unreported_leftovers(
    capabilities: &[String],
    connection_id: u64,
    broadcast_tx: &broadcast::Sender<ReceivedServerMessage>,
) {
    if capabilities
        .iter()
        .any(|c| c == snitchwatch_bridge::bridge_capabilities::BLOCKLIST_LEFTOVERS)
    {
        return;
    }
    let _ = broadcast_tx.send(ReceivedServerMessage {
        connection_id,
        message: ServerMessage::SetBlocklistLeftovers {
            count: 0,
            cause: None,
            reason: None,
        },
    });
}
