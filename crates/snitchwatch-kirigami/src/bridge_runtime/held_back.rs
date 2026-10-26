//! How many desktop notices of each flow the notice gate held back, for the
//! flow's newest pending row (0d part 2). GUI-local, never on the wire.
//!
//! The notification controller copies the gate's count here after every
//! decision for a flow, and removes it when the flow is cleared; the
//! connections model reads it on its pending-age tick. Each entry keeps the
//! end of its repeat window, so a count reads 0 once the window is over
//! even if no new ask comes to overwrite it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::BridgeHandles;
use crate::notifier::FlowKey;

/// Shared by the controller (writer) and the model (reader); cheap to clone.
#[derive(Clone, Default)]
pub struct HeldBackNotices(Arc<Mutex<HashMap<FlowKey, HeldBack>>>);

#[derive(Debug, Clone, Copy)]
struct HeldBack {
    count: u32,
    until: Instant,
}

impl HeldBackNotices {
    /// Copy the gate's `(count, window end)` for `flow`: no entry, a zero
    /// count or an ended window removes it. Every ended entry goes too.
    pub fn record(&self, flow: &FlowKey, held: Option<(u32, Instant)>, now: Instant) {
        let mut map = self.lock();
        map.retain(|_, entry| entry.until > now);
        match held {
            Some((count, until)) if count > 0 && until > now => {
                map.insert(flow.clone(), HeldBack { count, until });
            }
            _ => {
                map.remove(flow);
            }
        }
    }

    /// The flow's notice ended in a way that lets its next ask notify.
    pub fn remove(&self, flow: &FlowKey) {
        self.lock().remove(flow);
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

    #[test]
    fn a_count_reads_zero_once_its_window_ends() {
        let notices = HeldBackNotices::default();
        let t0 = Instant::now();
        let until = t0 + Duration::from_secs(60);
        assert_eq!(notices.count(&curl("a.example"), t0), 0);
        notices.record(&curl("a.example"), Some((3, until)), t0);
        assert_eq!(notices.count(&curl("a.example"), t0), 3);
        assert_eq!(notices.count(&curl("a.example"), until), 0);
        assert_eq!(notices.count(&curl("b.example"), t0), 0);
    }

    #[test]
    fn a_zero_count_no_entry_or_a_removal_leaves_nothing() {
        let notices = HeldBackNotices::default();
        let t0 = Instant::now();
        let until = t0 + Duration::from_secs(60);
        for held in [Some((0, until)), None] {
            notices.record(&curl("a.example"), Some((2, until)), t0);
            notices.record(&curl("a.example"), held, t0);
            assert_eq!(notices.count(&curl("a.example"), t0), 0);
        }
        notices.record(&curl("a.example"), Some((2, until)), t0);
        notices.remove(&curl("a.example"));
        assert_eq!(notices.count(&curl("a.example"), t0), 0);
    }

    /// Entries past their window go on the next write, as the gate prunes.
    #[test]
    fn a_write_drops_every_ended_entry() {
        let notices = HeldBackNotices::default();
        let t0 = Instant::now();
        notices.record(
            &curl("old.example"),
            Some((1, t0 + Duration::from_secs(10))),
            t0,
        );
        let later = t0 + Duration::from_secs(20);
        notices.record(
            &curl("new.example"),
            Some((1, later + Duration::from_secs(10))),
            later,
        );
        assert_eq!(notices.len(), 1);
        assert_eq!(notices.count(&curl("new.example"), later), 1);
    }
}
