//! Connection history PR 3a: the pin registry (design W1). Every accepted but
//! uncommitted item takes a pin recording its arrival time; the watermark a
//! commit may write is `min(commit time, oldest pin)`. Pure in-memory; the
//! "release in the same transaction" half is PR 3b.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Source of per-registry identities, so a handle only works on the registry
/// that issued it.
static NEXT_REGISTRY: AtomicU64 = AtomicU64::new(0);

/// Opaque pin handle: not `Clone`, fields private, only [`PinRegistry::add`]
/// builds one. It carries its registry's identity; ids are never reused, so a
/// stale handle is always detected as unknown.
#[derive(Debug, PartialEq, Eq)]
pub struct PinId {
    registry: u64,
    id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// Released twice (this registry issued it, but it is no longer live).
    #[error("unknown pin")]
    Unknown,
    /// Issued by a different registry. Nothing was released.
    #[error("pin belongs to another registry")]
    Foreign,
    /// More than `u64::MAX` pins were issued.
    #[error("pin ids exhausted")]
    Exhausted,
}

#[derive(Debug, Default)]
struct Inner {
    next_id: u64,
    arrival_by_id: BTreeMap<u64, i64>,
    /// `(arrival, id)`, so the first element is the oldest pin.
    by_arrival: BTreeSet<(i64, u64)>,
}

#[derive(Debug)]
pub struct PinRegistry {
    registry: u64,
    inner: Mutex<Inner>,
}

impl Default for PinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PinRegistry {
    pub fn new() -> Self {
        Self::with_inner(Inner::default())
    }

    fn with_inner(inner: Inner) -> Self {
        Self {
            // Wraps only after 2^64 registries.
            registry: NEXT_REGISTRY.fetch_add(1, Ordering::Relaxed),
            inner: Mutex::new(inner),
        }
    }

    // Every mutation is two infallible map operations, so a poisoned lock
    // never guards a half-updated state; recover it.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a pin that arrived at `arrival_ms`.
    pub fn add(&self, arrival_ms: i64) -> Result<PinId, PinError> {
        let mut inner = self.lock();
        let next = inner.next_id.checked_add(1).ok_or(PinError::Exhausted)?;
        let id = inner.next_id;
        inner.next_id = next;
        inner.arrival_by_id.insert(id, arrival_ms);
        inner.by_arrival.insert((arrival_ms, id));
        Ok(PinId {
            registry: self.registry,
            id,
        })
    }

    /// Releases a pin. A second release is `Unknown`, a handle from another
    /// registry is `Foreign`; either way nothing changes.
    pub fn release(&self, pin: &PinId) -> Result<(), PinError> {
        if pin.registry != self.registry {
            return Err(PinError::Foreign);
        }
        let mut inner = self.lock();
        let arrival = inner
            .arrival_by_id
            .remove(&pin.id)
            .ok_or(PinError::Unknown)?;
        inner.by_arrival.remove(&(arrival, pin.id));
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
        Self::with_inner(Inner {
            next_id,
            ..Inner::default()
        })
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
