//! Translate opensnitchd Notification events into LS WebSocket ServerMessages.
//!
//! This is intentionally minimal in Plan 1. The exact shape of opensnitchd
//! notifications is something M0 is still teaching us; we wire the plumbing
//! and leave the full match arms for follow-up once we have recorded traffic.

use crate::ws_messages::{ConnectionRow, ServerMessage};
use snitchwatch_proto::protocol::Notification;

/// Outcome of translating one Notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Translated {
    /// Push these messages to the WS broadcast.
    Messages(Vec<ServerMessage>),
    /// This notification was an AskRule and the bridge should call
    /// `cache.insert_pending` with the produced row, then await the
    /// resulting oneshot before replying.
    AskRule(ConnectionRow),
    /// Notification not relevant to the UI; ignore.
    Ignored,
}

/// Translate one Notification. The exact discriminator is the `type` field
/// on the generated `Notification` struct (mapped from proto `Action type`).
/// Until we have recorded traffic samples, we only implement `Ignored`.
pub fn translate_notification(_n: &Notification) -> Translated {
    Translated::Ignored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_notification_is_ignored() {
        let n = Notification::default();
        assert_eq!(translate_notification(&n), Translated::Ignored);
    }
}
