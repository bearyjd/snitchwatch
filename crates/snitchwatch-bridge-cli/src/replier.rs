//! Where an answer to a GUI request goes: the connection that asked (its
//! `ReplyTo`, stamped by `ws_server`), or the broadcast for an in-process
//! sender. Shared by rule import/export (P2.7) and rule commands (P2.1).

use snitchwatch_bridge::translator::verdict::strip_display_hazards;
use snitchwatch_bridge::ws_messages::{ReplyTo, ServerMessage};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

/// Longest daemon error text shown.
const MAX_REASON_CHARS: usize = 200;

/// Daemon text for a plain-text label: no hidden characters, not too long.
pub(crate) fn display_reason(text: &str) -> String {
    let shown = strip_display_hazards(text);
    if shown.chars().count() <= MAX_REASON_CHARS {
        return shown;
    }
    let mut short: String = shown.chars().take(MAX_REASON_CHARS).collect();
    short.push('…');
    short
}

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
    pub(crate) async fn send_final(&self, message: ServerMessage) {
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
