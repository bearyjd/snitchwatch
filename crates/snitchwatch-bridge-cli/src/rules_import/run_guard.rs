//! Where an import's answers go, and the guard that ends an apply however
//! it ends (P2.7 review #7, #8).

use snitchwatch_bridge::cache::rules::PublishHold;
use snitchwatch_bridge::ws_messages::{ReplyTo, ServerMessage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

use super::apply::Totals;

/// How long one answer may wait for room on its connection's queue before
/// it is dropped (a GUI that stopped reading).
const REPLY_WAIT: Duration = Duration::from_secs(5);

/// Where an answer goes: the connection that asked, or the broadcast for an
/// in-process sender (which has no connection).
/// A GUI that stops reading costs one [`REPLY_WAIT`]: after that, its
/// answers are dropped without waiting, so an apply's progress can't stall
/// the import (and the rule list held for it) for everyone else.
#[derive(Clone)]
pub(crate) struct Replier {
    reply: Option<ReplyTo>,
    broadcast: broadcast::Sender<ServerMessage>,
    gave_up: Arc<AtomicBool>,
}

impl Replier {
    pub(crate) fn new(reply: Option<ReplyTo>, broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self {
            reply,
            broadcast,
            gave_up: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn broadcast(broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self::new(None, broadcast)
    }

    pub(crate) async fn send(&self, message: ServerMessage) {
        match &self.reply {
            Some(reply) if self.gave_up.load(Ordering::SeqCst) => {
                let _ = reply.0.try_send(message);
            }
            Some(reply) => match tokio::time::timeout(REPLY_WAIT, reply.send(message)).await {
                // `Ok(false)`: the connection is gone; nobody to tell.
                Ok(_) => {}
                Err(_) => {
                    warn!("a GUI isn't reading its import answers; no longer waiting for it");
                    self.gave_up.store(true, Ordering::SeqCst);
                }
            },
            None => {
                let _ = self.broadcast.send(message);
            }
        }
    }

    /// Without waiting (from `Drop`).
    fn send_now(&self, message: ServerMessage) {
        match &self.reply {
            Some(reply) => {
                if reply.0.try_send(message).is_err() {
                    warn!("an import result couldn't be queued for its GUI");
                }
            }
            None => {
                let _ = self.broadcast.send(message);
            }
        }
    }
}

/// Owns a running apply's state. Dropped when the apply finishes, or when
/// its task panics or is cancelled: it publishes the held rule list once,
/// frees the import for the next apply, and sends a `RulesImportResult` with
/// the totals it has.
pub(crate) struct ApplyRun {
    running: Arc<AtomicBool>,
    hold: Option<PublishHold>,
    replier: Replier,
    preview_id: String,
    pub(crate) totals: Totals,
}

impl ApplyRun {
    pub(crate) fn new(
        running: Arc<AtomicBool>,
        hold: PublishHold,
        replier: Replier,
        preview_id: String,
    ) -> Self {
        Self {
            running,
            hold: Some(hold),
            replier,
            preview_id,
            totals: Totals::default(),
        }
    }
}

impl Drop for ApplyRun {
    fn drop(&mut self) {
        // One `SetRules` with every confirmed rule, before the result.
        drop(self.hold.take());
        self.running.store(false, Ordering::SeqCst);
        self.replier.send_now(ServerMessage::RulesImportResult {
            preview_id: self.preview_id.clone(),
            applied: self.totals.applied,
            rejected: self.totals.rejected,
            not_sent: self.totals.not_sent,
            no_answer: self.totals.no_answer,
        });
    }
}
