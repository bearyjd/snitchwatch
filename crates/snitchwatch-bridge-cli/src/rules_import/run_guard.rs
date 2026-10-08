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
/// A GUI that stops reading costs one [`REPLY_WAIT`] per connection (the
/// mark lives on its `ReplyTo`): after that, its answers are dropped without
/// waiting, so an apply's progress can't stall the import (and the rule
/// list held for it) for everyone else.
#[derive(Clone)]
pub(crate) struct Replier {
    reply: Option<ReplyTo>,
    broadcast: broadcast::Sender<ServerMessage>,
}

/// How long a final result may wait for room on its connection's queue.
const RESULT_WAIT: Duration = Duration::from_secs(2);

impl Replier {
    pub(crate) fn new(reply: Option<ReplyTo>, broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self { reply, broadcast }
    }

    #[cfg(test)]
    pub(crate) fn broadcast(broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self::new(None, broadcast)
    }

    pub(crate) async fn send(&self, message: ServerMessage) {
        match &self.reply {
            Some(reply) if reply.stalled() => {
                let _ = reply.try_send(message);
            }
            Some(reply) => match tokio::time::timeout(REPLY_WAIT, reply.send(message)).await {
                // `Ok(false)`: the connection is gone; nobody to tell.
                Ok(_) => {}
                Err(_) => {
                    warn!("a GUI isn't reading its import answers; no longer waiting for it");
                    reply.mark_stalled();
                }
            },
            None => {
                let _ = self.broadcast.send(message);
            }
        }
    }

    /// Without waiting: dropped if the connection's queue is full.
    pub(crate) fn send_now(&self, message: ServerMessage) {
        match &self.reply {
            Some(reply) => {
                if !reply.try_send(message) {
                    warn!("an answer couldn't be queued for its GUI; dropped");
                }
            }
            None => {
                let _ = self.broadcast.send(message);
            }
        }
    }

    /// A final answer: waits up to [`RESULT_WAIT`] for room, even on a
    /// connection that stalled before.
    async fn send_final(&self, message: ServerMessage) {
        match &self.reply {
            Some(reply) => {
                if !matches!(
                    tokio::time::timeout(RESULT_WAIT, reply.send(message)).await,
                    Ok(true)
                ) {
                    warn!("an import result couldn't be delivered to its GUI");
                }
            }
            None => {
                let _ = self.broadcast.send(message);
            }
        }
    }
}

/// Owns a running apply's state. [`finish`](Self::finish) ends it; so does
/// dropping it (its task panicked or was cancelled): either way the held
/// rule list is published once, the import is freed for the next apply, and
/// a `RulesImportResult` with the totals so far is sent, waiting briefly for
/// room rather than being dropped.
pub(crate) struct ApplyRun {
    running: Arc<AtomicBool>,
    hold: Option<PublishHold>,
    replier: Replier,
    preview_id: String,
    pub(crate) totals: Totals,
    finished: bool,
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
            finished: false,
        }
    }

    /// Publish, free the import, and send the result.
    pub(crate) async fn finish(mut self) {
        let result = self.end();
        self.replier.send_final(result).await;
    }

    /// Publish the held rules and free the import; the result to send.
    fn end(&mut self) -> ServerMessage {
        self.finished = true;
        // One `SetRules` with every confirmed rule, before the result.
        drop(self.hold.take());
        self.running.store(false, Ordering::SeqCst);
        ServerMessage::RulesImportResult {
            preview_id: self.preview_id.clone(),
            applied: self.totals.applied,
            rejected: self.totals.rejected,
            not_sent: self.totals.not_sent,
            no_answer: self.totals.no_answer,
        }
    }
}

impl Drop for ApplyRun {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let result = self.end();
        // Ended abnormally: still deliver the result, off this stack.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let replier = self.replier.clone();
                runtime.spawn(async move { replier.send_final(result).await });
            }
            Err(_) => self.replier.send_now(result),
        }
    }
}
