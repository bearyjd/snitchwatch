//! What the sequencer needs from the bridge, as a trait, so it runs against
//! a fake in tests and against [`super::bridge_sink::BridgeSink`] for real.

use std::time::Duration;

use snitchwatch_bridge::ws_messages::ServerMessage;

use crate::learning_review::RulePayload;

/// What one wait on the bridge's feed produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Message(ServerMessage),
    /// Nothing arrived in time.
    Idle,
    /// The feed dropped messages: what the sequencer knows may be stale.
    Lagged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The queue is full: try again shortly.
    Full,
    /// No live session to send on.
    NotConnected,
}

/// The session the batch started on ended (a reconnect or a stop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLost;

pub trait RuleSink {
    /// A fresh `AddRule` request id.
    fn new_request_id(&mut self) -> String;
    /// Queue one `AddRule` with `request_id` (never blocks).
    fn send_add(&mut self, request_id: &str, rule: &RulePayload) -> Result<(), SendError>;
    /// Queue a `RequestSnapshot`.
    fn request_snapshot(&mut self) -> Result<(), SendError>;
    /// The next message from the bridge, waiting at most `wait`.
    fn next_message(&mut self, wait: Duration) -> Result<Incoming, SessionLost>;
    fn sleep(&mut self, duration: Duration);
    /// A monotonic clock.
    fn now(&self) -> Duration;
    /// Whether the owner pressed Cancel.
    fn cancelled(&self) -> bool;
}

/// The waits and limits of a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// How long a rules list may take to arrive.
    pub list_wait: Duration,
    /// How long one rule's result may take (the bridge times out sooner).
    pub result_wait: Duration,
    pub poll: Duration,
    pub backoff_start: Duration,
    pub backoff_max: Duration,
    /// Tries for one full-queue send before the batch stops.
    pub send_attempts: u32,
    /// Timeouts in a row that stop the batch.
    pub max_consecutive_timeouts: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            list_wait: Duration::from_secs(5),
            result_wait: Duration::from_secs(30),
            poll: Duration::from_millis(250),
            backoff_start: Duration::from_millis(50),
            backoff_max: Duration::from_secs(1),
            send_attempts: 8,
            max_consecutive_timeouts: 3,
        }
    }
}
