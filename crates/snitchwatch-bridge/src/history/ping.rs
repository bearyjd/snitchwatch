//! The `Ping` handler's taps into history's loss accounting: the daemon's
//! down and up edges, and the two steps of a ping's own losses.
//!
//! A ping is pinned when it **begins** (everything it needs from the shared
//! state is taken then, and carried) and its predecessor's guard is released
//! only when it **finishes**, after the events it carried are queued: a crash
//! in between must still find the earlier guard holding the watermark.

use std::sync::Arc;

use super::capture::{Capture, Shared};
use super::gaps::PingBegun;
use crate::cache::rule_hits::UpstreamLoss;

/// One ping between [`Capture::begin_ping`] and [`PingTicket::finish`]. A
/// ticket dropped unfinished (the handler was cancelled) finishes itself,
/// counting the ping's events as lost.
pub struct PingTicket {
    shared: Arc<Shared>,
    begun: Option<PingBegun>,
}

impl PingTicket {
    /// The ping's events are queued: tally what it showed was never received
    /// and release its predecessor's guard.
    pub fn finish(mut self) {
        self.settle(false);
    }

    fn settle(&mut self, cancelled: bool) {
        let Some(begun) = self.begun.take() else {
            return;
        };
        let begun = if cancelled { begun.cancelled() } else { begun };
        self.shared
            .with_losses(|l, registry| l.finish_ping(registry, begun));
    }
}

impl Drop for PingTicket {
    fn drop(&mut self) {
        self.settle(true);
    }
}

impl Capture {
    /// The daemon went away: a `daemon-disconnected` gap opens now. In
    /// memory only; the watchdog calls it on its down edge.
    pub fn daemon_down(&self) {
        self.shared
            .with_losses(|l, registry| l.daemon_down(registry));
    }

    /// The daemon is back: the gap closes now.
    pub fn daemon_up(&self) {
        self.shared
            .with_losses(|l, registry| l.daemon_up(registry.now()));
    }

    /// A ping reached the bridge, carrying `events` events: call before
    /// anything awaits, and [`PingTicket::finish`] once they are queued.
    pub fn begin_ping(&self, loss: UpstreamLoss, events: u64) -> PingTicket {
        let mut begun = None;
        self.shared.number_acceptance();
        self.shared
            .with_losses(|l, registry| begun = Some(l.begin_ping(registry, loss, events)));
        PingTicket {
            shared: self.shared.clone(),
            begun,
        }
    }

    /// What one ping showed was never received (`upstream-loss`), with no
    /// events to wait for. In memory only.
    pub fn observe_upstream_loss(&self, loss: UpstreamLoss) {
        self.begin_ping(loss, 0).finish();
    }
}
