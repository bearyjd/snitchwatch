use super::{BridgeNotice, BridgePauseState, BridgeTrayState};
use snitchwatch_bridge::ws_messages::ServerMessage;

/// A service message labelled with the authenticated WebSocket session that
/// received it. Qt delivery is asynchronous, so consumers must retain this
/// label until their queued Qt callback runs; a callback from an older service
/// instance must not mutate a model after reconnect.
#[derive(Clone, Debug)]
pub struct ReceivedServerMessage {
    pub connection_id: u64,
    pub message: ServerMessage,
}

/// Session-labelled shell state, subject to the same queued-Qt stale-frame
/// guard as model messages.
#[derive(Clone, Debug)]
pub struct ReceivedTrayState {
    pub connection_id: u64,
    pub state: BridgeTrayState,
}

/// Session-labelled filter-pause state (issue #47), routed and guarded the
/// same way as [`ReceivedTrayState`].
#[derive(Clone, Debug)]
pub struct ReceivedPauseState {
    pub connection_id: u64,
    pub state: BridgePauseState,
}

/// The bridge's last `PromptSlot` (prompt-slot plan, part A), session-
/// labelled and guarded like [`ReceivedTrayState`]. Empty until one arrives.
#[derive(Clone, Debug, Default)]
pub struct ReceivedPromptSlot {
    pub connection_id: u64,
    pub holder: Option<snitchwatch_bridge::prompt_slot::PromptSlotHolder>,
    pub holders: u32,
    pub defaulted_at_least: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ReceivedNotice {
    pub connection_id: u64,
    pub notice: BridgeNotice,
}
