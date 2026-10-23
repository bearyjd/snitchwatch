//! [`RuleSink`] over the live bridge handles.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use snitchwatch_bridge::ws_messages::ClientMessage;
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use super::sink::{Incoming, RuleSink, SendError, SessionLost};
use crate::bridge_runtime::{BridgeHandles, ReceivedServerMessage, SendClientMessageError};
use crate::learning_review::RulePayload;
use crate::rule_commands::next_request_id;

/// Pins itself to the first session that answers and treats a message from
/// another one as the end of the batch.
///
/// `next_message` blocks on the bridge's runtime, so use it from a worker
/// thread, not from a runtime thread.
pub struct BridgeSink {
    handles: BridgeHandles,
    receiver: broadcast::Receiver<ReceivedServerMessage>,
    session: Option<u64>,
    started: Instant,
    cancel: Arc<AtomicBool>,
}

impl BridgeSink {
    /// Subscribe before the first message is sent. Setting `cancel` ends
    /// the batch before its next rule.
    pub fn new(handles: BridgeHandles, cancel: Arc<AtomicBool>) -> Self {
        let receiver = handles.subscribe();
        Self {
            handles,
            receiver,
            session: None,
            started: Instant::now(),
            cancel,
        }
    }

    fn queue(&self, message: ClientMessage) -> Result<(), SendError> {
        let sent = match self.session {
            Some(id) => self.handles.try_send_for_session(id, message),
            None => self.handles.try_send(message),
        };
        sent.map_err(|error| match error {
            SendClientMessageError::Full => SendError::Full,
            _ => SendError::NotConnected,
        })
    }
}

impl RuleSink for BridgeSink {
    fn new_request_id(&mut self) -> String {
        next_request_id("bulk")
    }

    fn send_add(&mut self, request_id: &str, rule: &RulePayload) -> Result<(), SendError> {
        self.queue(ClientMessage::AddRule {
            rule: rule.to_wire(),
            request_id: Some(request_id.to_owned()),
            reply: None,
        })
    }

    fn request_snapshot(&mut self) -> Result<(), SendError> {
        self.queue(ClientMessage::RequestSnapshot)
    }

    fn next_message(&mut self, wait: Duration) -> Result<Incoming, SessionLost> {
        // `block_on` panics on a runtime thread; refuse instead.
        if Handle::try_current().is_ok() {
            return Err(SessionLost);
        }
        let receiver = &mut self.receiver;
        let received = self
            .handles
            .runtime()
            .block_on(async { tokio::time::timeout(wait, receiver.recv()).await });
        match received {
            Err(_elapsed) => Ok(Incoming::Idle),
            Ok(Err(RecvError::Lagged(_))) => Ok(Incoming::Lagged),
            Ok(Err(RecvError::Closed)) => Err(SessionLost),
            Ok(Ok(received)) => match self.session {
                Some(id) if id != received.connection_id => Err(SessionLost),
                _ => {
                    self.session = Some(received.connection_id);
                    Ok(Incoming::Message(received.message))
                }
            },
        }
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}
