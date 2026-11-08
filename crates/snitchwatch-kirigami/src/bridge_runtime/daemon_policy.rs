//! What the connected daemon refuses (`ServerMessage::DaemonPolicy`, sent by
//! a bridge that advertises `bridge_capabilities::DAEMON_POLICY_V1`), kept by
//! the runtime for the live session.
//!
//! The bridge sends it once right after `Authenticated` and again whenever
//! the daemon's version changes, so a model or controller created later
//! would never see it on the feed: it reads it here instead. A session whose
//! bridge didn't advertise the capability keeps the default (v1.8.0's rules),
//! and every new session starts from the default.

use std::sync::Mutex;

use snitchwatch_bridge::rule_policy::PolicyContext;
use snitchwatch_bridge::ws_messages::ServerMessage;

use super::{BridgeHandles, ConnectionState};

/// One session's daemon policy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionPolicy {
    /// The session's bridge advertised `DAEMON_POLICY_V1`.
    advertised: bool,
    /// Its latest `DaemonPolicy`; the default until one arrives.
    context: PolicyContext,
}

/// Record whether session `connection_id`'s bridge advertised the
/// capability. A session that has been replaced meanwhile is left alone.
pub(super) fn mark_advertised(
    connection: &Mutex<ConnectionState>,
    connection_id: u64,
    advertised: bool,
) {
    let mut state = connection.lock().unwrap_or_else(|p| p.into_inner());
    if state.connection_id == connection_id {
        state.daemon_policy = SessionPolicy {
            advertised,
            context: PolicyContext::default(),
        };
    }
}

/// Keep session `connection_id`'s latest `DaemonPolicy`. Ignored from a
/// session that has been replaced, or whose bridge didn't advertise it.
pub(super) fn observe(
    connection: &Mutex<ConnectionState>,
    connection_id: u64,
    message: &ServerMessage,
) {
    let ServerMessage::DaemonPolicy(policy) = message else {
        return;
    };
    let mut state = connection.lock().unwrap_or_else(|p| p.into_inner());
    if state.connection_id != connection_id || !state.daemon_policy.advertised {
        return;
    }
    state.daemon_policy = SessionPolicy {
        context: PolicyContext {
            refuse_source_network: policy.refuse_source_network,
        },
        ..state.daemon_policy
    };
}

impl BridgeHandles {
    /// What the live session's daemon refuses; the default (nothing beyond
    /// v1.8.0's rules) while disconnected or on a bridge without
    /// `DAEMON_POLICY_V1`.
    pub fn daemon_policy(&self) -> PolicyContext {
        let state = self.connection.lock().unwrap_or_else(|p| p.into_inner());
        if state.connected {
            state.daemon_policy.context
        } else {
            PolicyContext::default()
        }
    }
}
