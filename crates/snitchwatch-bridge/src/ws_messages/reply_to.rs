//! The per-connection reply channel, re-exported from `ws_messages`.

use super::ServerMessage;

/// A channel back to one WebSocket connection, stamped on rule import and
/// export requests by `ws_server` so their answers reach only the GUI that
/// asked. Never serialized. Clones share the connection's "stopped reading"
/// mark, so a GUI that stops reading is waited on once, not per request.
#[derive(Clone)]
pub struct ReplyTo {
    tx: tokio::sync::mpsc::Sender<ServerMessage>,
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ReplyTo {
    /// One connection's channel (call once per connection, then clone).
    pub fn new(tx: tokio::sync::mpsc::Sender<ServerMessage>) -> Self {
        Self {
            tx,
            stalled: std::sync::Arc::default(),
        }
    }

    /// Deliver `message`, waiting for room; `false` when the connection is
    /// gone.
    pub async fn send(&self, message: ServerMessage) -> bool {
        // Artifacts use ExportSession's one expiring slot exclusively.
        if matches!(&message, ServerMessage::CacheSummaryExport { .. }) {
            return false;
        }
        self.tx.send(message).await.is_ok()
    }

    /// Deliver `message` only if there is room now.
    pub fn try_send(&self, message: ServerMessage) -> bool {
        if matches!(&message, ServerMessage::CacheSummaryExport { .. }) {
            return false;
        }
        self.tx.try_send(message).is_ok()
    }

    /// Whether the connection was found not reading its answers.
    pub fn stalled(&self) -> bool {
        self.stalled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn mark_stalled(&self) {
        self.stalled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The connection took an answer again: it is reading after all.
    pub fn clear_stalled(&self) {
        self.stalled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for ReplyTo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplyTo")
    }
}

impl PartialEq for ReplyTo {
    fn eq(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }
}
