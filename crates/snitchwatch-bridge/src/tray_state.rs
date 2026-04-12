//! Bridge-owned tray state publisher.
//!
//! The bridge is the source of truth for what the tray icon should show. The
//! Tauri shell subscribes to `TrayStatePublisher::subscribe()` and re-renders
//! on every change. Headless tests can assert transitions without Tauri.

use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TrayState {
    #[default]
    Idle,
    Pending(usize),
    RecentBlock { what: String, ttl: Duration },
    FilterOff,
    DaemonDown,
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
