//! Bridge-owned tray state publisher.
//!
//! The bridge is the source of truth for what the tray icon should show. The
//! Tauri shell subscribes to `TrayStatePublisher::subscribe()` and re-renders
//! on every change. Headless tests can assert transitions without Tauri.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayState {
    #[default]
    Idle,
    Pending(usize),
    RecentBlock {
        what: String,
        ttl: Duration,
    },
    FilterOff,
    DaemonDown,
}

impl TrayState {
    /// What the tray shows for the bridge's inputs (issue #58), by priority:
    /// `DaemonDown` > `FilterOff` (paused) > `Pending(n)` > `Idle`. The one
    /// place this is decided: every resync publishes it, so none can hide a
    /// daemon outage or a pause. `RecentBlock` is a transient overlay set
    /// directly, and its revert comes back here.
    pub fn derive(daemon_down: bool, paused: bool, pending: usize) -> Self {
        if daemon_down {
            Self::DaemonDown
        } else if paused {
            Self::FilterOff
        } else if pending == 0 {
            Self::Idle
        } else {
            Self::Pending(pending)
        }
    }
}

pub struct TrayStatePublisher {
    tx: watch::Sender<TrayState>,
}

impl TrayStatePublisher {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(TrayState::Idle);
        Self { tx }
    }

    pub fn subscribe(&self) -> watch::Receiver<TrayState> {
        self.tx.subscribe()
    }

    pub fn set(&self, state: TrayState) {
        // send_replace ignores the no-receivers error: state still updates
        // for late subscribers.
        let _ = self.tx.send_replace(state);
    }
}

impl Default for TrayStatePublisher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_follows_daemon_down_then_pause_then_pending_then_idle() {
        for paused in [false, true] {
            for pending in [0, 3] {
                assert_eq!(
                    TrayState::derive(true, paused, pending),
                    TrayState::DaemonDown,
                    "down, paused {paused}, {pending} pending"
                );
            }
        }
        for pending in [0, 3] {
            assert_eq!(
                TrayState::derive(false, true, pending),
                TrayState::FilterOff,
                "paused, {pending} pending"
            );
        }
        assert_eq!(TrayState::derive(false, false, 3), TrayState::Pending(3));
        assert_eq!(TrayState::derive(false, false, 0), TrayState::Idle);
    }

    #[tokio::test]
    async fn publisher_starts_idle_and_propagates_pending_count() {
        let pub_ = TrayStatePublisher::new();
        let mut rx = pub_.subscribe();
        assert_eq!(*rx.borrow(), TrayState::Idle);

        pub_.set(TrayState::Pending(3));
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(3));

        pub_.set(TrayState::DaemonDown);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown);
    }
}
