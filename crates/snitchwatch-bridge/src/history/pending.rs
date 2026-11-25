//! The prompts accepted and not yet settled (PR 10, #420).
//!
//! A [`DecisionGuard`](super::guard::DecisionGuard) hands its arrival pin
//! here when the prompt is accepted and gets a key back. Settling takes the
//! pin back out; a control operation or the end of the capture takes **all**
//! of them at once and turns them into one gap. A pin therefore leaves this
//! registry only into an event row or into a gap tally, never by a discard
//! alone (W1), and a guard that settles after its pin was taken finds
//! nothing and writes nothing.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::pins::PinId;

/// Names one pending prompt. Not an identity anyone else can forge: it is
/// only handed to the guard that registered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PendingKey(u64);

#[derive(Default)]
struct Inner {
    next: u64,
    by_key: BTreeMap<u64, (PinId, i64)>,
}

#[derive(Default)]
pub(super) struct PendingGuards(Mutex<Inner>);

impl PendingGuards {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn add(&self, pin: PinId, arrival_ms: i64) -> PendingKey {
        let mut inner = self.lock();
        let key = inner.next;
        inner.next += 1;
        inner.by_key.insert(key, (pin, arrival_ms));
        PendingKey(key)
    }

    /// The pin of a prompt that settles now; `None` when a gap already took it.
    pub(super) fn take(&self, key: PendingKey) -> Option<PinId> {
        self.lock().by_key.remove(&key.0).map(|(pin, _)| pin)
    }

    /// Every pending pin, earliest arrival first, leaving the registry empty.
    pub(super) fn drain(&self) -> Vec<(PinId, i64)> {
        let mut all: Vec<(PinId, i64)> = std::mem::take(&mut self.lock().by_key)
            .into_values()
            .collect();
        all.sort_by_key(|(_, arrival)| *arrival);
        all
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.lock().by_key.len()
    }
}
