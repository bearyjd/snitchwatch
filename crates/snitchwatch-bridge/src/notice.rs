//! Bridge-owned desktop notification bus.
//!
//! The Tauri shell subscribes to a broadcast::Receiver<Notice> and dispatches
//! each entry to `notify-rust`. Headless tests use the receiver directly and
//! never touch D-Bus.

use tokio::sync::broadcast;

const BUS_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Pending {
        row_id: u64,
        process: String,
    },
    DaemonAway,
    FilterPauseExpired,
    /// A `Deny` verdict's requested scope (`AnyHostOnDomain`/`AnyHost`)
    /// couldn't be honored and silently narrowed to an exact-host match —
    /// see `translator::verdict::scope_degradation` and issue #14's
    /// security review FIX 2. Unlike a narrowed `Allow` (fail-safe, silent
    /// by design), a narrowed `Deny` under-blocks relative to what the
    /// pending-decision dialog offered.
    ///
    /// This alone does NOT satisfy "the client must be told": this bus is
    /// consumed only by the desktop notifiers below (`notify-rust`), so a
    /// headless `bridge-cli`, an unattended GUI, or any session with no
    /// D-Bus notification server gets no signal from this variant at all.
    /// The actual client-facing signal is
    /// `ws_messages::ServerMessage::DenyScopeNarrowed`, broadcast alongside
    /// this one from `grpc_server::UiService::ask_rule` — round 2 of the
    /// issue #14 security review (HIGH) found an earlier version of this
    /// fix relied on this desktop notice alone. `what`/`reason` here are
    /// display-boundary-sanitized before construction — see
    /// `translator::verdict::sanitize_for_display`/`ScopeDegradation`.
    DenyScopeNarrowed {
        row_id: u64,
        what: String,
        reason: String,
    },
}

pub struct NoticeBus {
    tx: broadcast::Sender<Notice>,
}

impl NoticeBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.tx.subscribe()
    }

    pub fn send(&self, notice: Notice) {
        // SendError when there are zero subscribers is expected and benign.
        let _ = self.tx.send(notice);
    }
}

impl Default for NoticeBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_delivers_to_two_subscribers() {
        let bus = NoticeBus::new();
        let mut rx_a = bus.subscribe();
        let mut rx_b = bus.subscribe();

        bus.send(Notice::DaemonAway);

        let got_a = rx_a.recv().await.unwrap();
        let got_b = rx_b.recv().await.unwrap();
        assert_eq!(got_a, Notice::DaemonAway);
        assert_eq!(got_b, Notice::DaemonAway);
    }

    #[tokio::test]
    async fn no_subscribers_does_not_panic() {
        let bus = NoticeBus::new();
        // No one is listening — send should silently no-op.
        bus.send(Notice::FilterPauseExpired);
    }
}
