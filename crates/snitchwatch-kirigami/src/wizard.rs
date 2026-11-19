//! Onboarding observes the GUI's authenticated bridge connection. The root
//! daemon's health is reported separately by the bridge diagnostics feed;
//! desktop processes must never probe its private gRPC socket.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Connected,
    UnreachableRetrying,
}

pub fn state_from_authenticated_bridge(connected: bool) -> DaemonState {
    if connected {
        DaemonState::Connected
    } else {
        DaemonState::UnreachableRetrying
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_bridge_is_connected_and_loss_remains_retryable() {
        assert_eq!(
            state_from_authenticated_bridge(true),
            DaemonState::Connected
        );
        assert_eq!(
            state_from_authenticated_bridge(false),
            DaemonState::UnreachableRetrying
        );
    }
}
