//! How many desktop notices of each flow the notice gate held back, for the
//! flow's newest pending row (0d part 2). GUI-local, never on the wire.
//!
//! The notification controller copies the gate's count here after every
//! decision for a flow, and the clear when the flow is cleared; the
//! connections model reads it on its refresh tick. Each entry keeps the
//! end of its repeat window, so a count reads 0 once the window is over
//! even if no new ask comes to overwrite it, and the gate's epoch, so a
//! copy read before a clear but written after it is refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::BridgeHandles;
use crate::notifier::{FlowKey, HeldBack};

/// Shared by the controller (writer) and the model (reader); cheap to clone.
#[derive(Clone, Default)]
pub struct HeldBackNotices(Arc<Mutex<HashMap<FlowKey, HeldBack>>>);

impl HeldBackNotices {
    /// Copy what the gate read for `flow` (a posted notice's count, or a
    /// clear's 0), unless a newer epoch is already here or its window has
    /// ended. `None` (nothing live in the gate) changes nothing. Every
    /// ended entry goes.
    pub fn record(&self, flow: &FlowKey, held: Option<HeldBack>, now: Instant) {
        let mut map = self.lock();
        map.retain(|_, entry| entry.until > now);
        let Some(held) = held else {
            return;
        };
        let newer_seen = map.get(flow).is_some_and(|seen| seen.epoch > held.epoch);
        if held.until > now && !newer_seen {
            map.insert(flow.clone(), held);
        }
    }

    /// Notices of `flow` held back within its current window; 0 after it.
    pub fn count(&self, flow: &FlowKey, now: Instant) -> u32 {
        self.lock()
            .get(flow)
            .filter(|entry| entry.until > now)
            .map_or(0, |entry| entry.count)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<FlowKey, HeldBack>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl BridgeHandles {
    /// This GUI's held-back notice counts (0d part 2).
    pub fn held_back_notices(&self) -> &HeldBackNotices {
        &self.held_back
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn curl(host: &str) -> FlowKey {
        FlowKey::of(1, Some("/usr/bin/curl"), "curl", host)
    }

    fn held(count: u32, until: Instant, epoch: u64) -> Option<HeldBack> {
        Some(HeldBack {
            count,
            until,
            epoch,
        })
    }

    #[test]
    fn a_count_reads_zero_once_its_window_ends() {
        let notices = HeldBackNotices::default();
        let t0 = Instant::now();
        let until = t0 + Duration::from_secs(60);
        assert_eq!(notices.count(&curl("a.example"), t0), 0);
        notices.record(&curl("a.example"), held(3, until, 1), t0);
        assert_eq!(notices.count(&curl("a.example"), t0), 3);
        assert_eq!(notices.count(&curl("a.example"), until), 0);
        assert_eq!(notices.count(&curl("b.example"), t0), 0);
    }

    /// Codex review of #230: a write read from the gate before a clear
    /// (an older epoch) never brings the cleared count back.
    #[test]
    fn an_older_epoch_never_overwrites_a_newer_one() {
        let notices = HeldBackNotices::default();
        let t0 = Instant::now();
        let until = t0 + Duration::from_secs(60);
        notices.record(&curl("a.example"), held(2, until, 1), t0);
        notices.record(&curl("a.example"), held(0, until, 2), t0);
        notices.record(&curl("a.example"), held(2, until, 1), t0);
        assert_eq!(notices.count(&curl("a.example"), t0), 0);
        // No read (the gate had no entry) changes nothing; a newer one counts.
        notices.record(&curl("a.example"), None, t0);
        notices.record(&curl("a.example"), held(1, until, 3), t0);
        assert_eq!(notices.count(&curl("a.example"), t0), 1);
    }

    /// Entries past their window go on the next write, as the gate prunes.
    #[test]
    fn a_write_drops_every_ended_entry() {
        let notices = HeldBackNotices::default();
        let t0 = Instant::now();
        notices.record(
            &curl("old.example"),
            held(1, t0 + Duration::from_secs(10), 1),
            t0,
        );
        let later = t0 + Duration::from_secs(20);
        notices.record(
            &curl("new.example"),
            held(1, later + Duration::from_secs(10), 2),
            later,
        );
        assert_eq!(notices.len(), 1);
        assert_eq!(notices.count(&curl("new.example"), later), 1);
    }
}
