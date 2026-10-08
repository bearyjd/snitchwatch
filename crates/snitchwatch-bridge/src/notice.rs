//! Bridge-owned desktop notification bus.
//!
//! The Tauri shell subscribes to a broadcast::Receiver<Notice> and dispatches
//! each entry to `notify-rust`. Headless tests use the receiver directly and
//! never touch D-Bus.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

const BUS_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The user asked to remember an answer, but the daemon reported no
    /// absolute executable path for the connection, so the bridge answered
    /// it once instead (issue #44; see
    /// `translator::process_binding::RuleRefusal`). For a user who answered
    /// with the window hidden; the client-facing signal is
    /// `ws_messages::ServerMessage::VerdictNotRemembered`, broadcast
    /// alongside. Carries no connection data: the explanation is
    /// `RuleRefusal::describe`'s fixed sentence.
    VerdictNotRemembered {
        row_id: u64,
    },
    /// A prompt that held opensnitchd's single prompt slot was released, and
    /// meanwhile at least `count` other connections got the firewall's
    /// default action (`prompt_slot`). Carries no connection data.
    PromptSlotSummary {
        row_id: u64,
        count: u64,
    },
}

/// The body of a `Notice::PromptSlotSummary`. Fixed text around a count.
pub fn prompt_slot_summary_text(count: u64) -> String {
    let noun = if count == 1 {
        "connection"
    } else {
        "connections"
    };
    format!(
        "While that prompt was open, at least {count} other {noun} got the firewall's \
         default action."
    )
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

    #[test]
    fn the_prompt_slot_summary_counts_at_least() {
        assert_eq!(
            prompt_slot_summary_text(1),
            "While that prompt was open, at least 1 other connection got the firewall's default \
             action."
        );
        assert!(prompt_slot_summary_text(7).contains("at least 7 other connections"));
    }

    #[tokio::test]
    async fn no_subscribers_does_not_panic() {
        let bus = NoticeBus::new();
        // No one is listening — send should silently no-op.
        bus.send(Notice::FilterPauseExpired);
    }
}
