//! Connection history PR 3a: the pin registry (design W1). Every accepted but
//! uncommitted item takes a pin recording its arrival time; the watermark a
//! commit may write is `min(commit time, oldest pin)`. Pure in-memory; the
//! "release in the same transaction" half is PR 3b.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

/// Opaque pin handle. Ids are never reused, so a stale handle is always
/// detected as unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PinId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// Released twice, or never issued by this registry.
    #[error("unknown pin")]
    Unknown,
    /// More than `u64::MAX` pins were issued.
    #[error("pin ids exhausted")]
    Exhausted,
}

#[derive(Debug, Default)]
struct Inner {
    next_id: u64,
    arrival_by_id: BTreeMap<PinId, i64>,
    /// `(arrival, id)`, so the first element is the oldest pin.
    by_arrival: BTreeSet<(i64, PinId)>,
}

#[derive(Debug, Default)]
pub struct PinRegistry(Mutex<Inner>);

impl PinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // Every mutation is two infallible map operations, so a poisoned lock
    // never guards a half-updated state; recover it.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a pin that arrived at `arrival_ms`.
    pub fn add(&self, arrival_ms: i64) -> Result<PinId, PinError> {
        let mut inner = self.lock();
        let next = inner.next_id.checked_add(1).ok_or(PinError::Exhausted)?;
        let id = PinId(inner.next_id);
        inner.next_id = next;
        inner.arrival_by_id.insert(id, arrival_ms);
        inner.by_arrival.insert((arrival_ms, id));
        Ok(id)
    }

    /// Releases a pin. A second release, or a foreign id, is an error.
    pub fn release(&self, id: PinId) -> Result<(), PinError> {
        let mut inner = self.lock();
        let arrival = inner.arrival_by_id.remove(&id).ok_or(PinError::Unknown)?;
        inner.by_arrival.remove(&(arrival, id));
        Ok(())
    }

    /// Arrival time of the oldest live pin, if any.
    pub fn oldest(&self) -> Option<i64> {
        self.lock().by_arrival.first().map(|(arrival, _)| *arrival)
    }

    /// The watermark a commit at `commit_ms` may record.
    pub fn watermark(&self, commit_ms: i64) -> i64 {
        self.oldest().map_or(commit_ms, |o| o.min(commit_ms))
    }

    pub fn len(&self) -> usize {
        self.lock().arrival_by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    pub(crate) fn with_next_id(next_id: u64) -> Self {
        Self(Mutex::new(Inner {
            next_id,
            ..Inner::default()
        }))
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
