//! Connection history PR 3a: the pin registry (design W1). Every accepted but
//! uncommitted item takes a pin recording its arrival time; the watermark a
//! commit may write is `min(commit time, oldest pin)`. In memory; the durable
//! half (PR 3b) is `durable`, which releases pins only after the transaction
//! that wrote their rows and their watermark committed.
//!
//! The clock is read **under the registry lock** by [`PinRegistry::accept`]
//! and [`PinRegistry::commit_watermark`], so a pin accepted after a watermark
//! was computed cannot have arrived before it (W1 without a race between
//! "read the time" and "register the pin").

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, as the registry's clock reads them.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The wall clock. A clock set before 1970 reads 0.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    })
}

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

pub struct PinRegistry {
    registry: u64,
    inner: Mutex<Inner>,
    clock: Clock,
}

impl std::fmt::Debug for PinRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinRegistry")
            .field("registry", &self.registry)
            .field("pins", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for PinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PinRegistry {
    pub fn new() -> Self {
        Self::with_clock(system_clock())
    }

    /// A registry reading `clock` for [`accept`](Self::accept) and
    /// [`commit_watermark`](Self::commit_watermark).
    pub fn with_clock(clock: Clock) -> Self {
        Self::with_inner(Inner::default(), clock)
    }

    fn with_inner(inner: Inner, clock: Clock) -> Self {
        Self {
            clock,
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
        Self::add_locked(&mut self.lock(), self.registry, arrival_ms)
    }

    /// The registry's clock, read now (not under the lock: for a time that
    /// does not need to order against pins, such as a session start).
    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Records a pin arriving now, the time read under the lock.
    pub fn accept(&self) -> Result<PinId, PinError> {
        self.accept_timed().map(|(pin, _)| pin)
    }

    /// [`accept`](Self::accept), also returning the arrival time it recorded.
    pub fn accept_timed(&self) -> Result<(PinId, i64), PinError> {
        let mut inner = self.lock();
        let now = (self.clock)();
        Self::add_locked(&mut inner, self.registry, now).map(|pin| (pin, now))
    }

    fn add_locked(inner: &mut Inner, registry: u64, arrival_ms: i64) -> Result<PinId, PinError> {
        let next = inner.next_id.checked_add(1).ok_or(PinError::Exhausted)?;
        let id = inner.next_id;
        inner.next_id = next;
        inner.arrival_by_id.insert(id, arrival_ms);
        inner.by_arrival.insert((arrival_ms, id));
        Ok(PinId { registry, id })
    }

    /// When `pin` arrived, if it is live and ours.
    pub(crate) fn arrival_of(&self, pin: &PinId) -> Option<i64> {
        if pin.registry != self.registry {
            return None;
        }
        self.lock().arrival_by_id.get(&pin.id).copied()
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

    /// The watermark a commit happening now may record, for a transaction that
    /// will release `released` together with writing it: `min(now, oldest pin
    /// that is not one of them)`. Time and pins are read under one lock.
    /// `released` is validated first, as [`release_all`](Self::release_all)
    /// will: a handle of another registry is `Foreign`, one that is not live
    /// or is listed twice is `Unknown`. Validated before the transaction, so
    /// the release after `COMMIT` cannot fail.
    pub fn commit_watermark(&self, released: &[&PinId]) -> Result<i64, PinError> {
        let inner = self.lock();
        self.validate_locked(&inner, released)?;
        let now = (self.clock)();
        let oldest_kept = inner
            .by_arrival
            .iter()
            .find(|(_, id)| !released.iter().any(|p| p.id == *id))
            .map(|(arrival, _)| *arrival);
        Ok(oldest_kept.map_or(now, |o| o.min(now)))
    }

    /// Whether `pins` could all be released now (see
    /// [`commit_watermark`](Self::commit_watermark)); changes nothing.
    pub fn check_release(&self, pins: &[&PinId]) -> Result<(), PinError> {
        self.validate_locked(&self.lock(), pins)
    }

    fn validate_locked(&self, inner: &Inner, pins: &[&PinId]) -> Result<(), PinError> {
        if pins.iter().any(|p| p.registry != self.registry) {
            return Err(PinError::Foreign);
        }
        let mut seen = BTreeSet::new();
        if !pins
            .iter()
            .all(|p| inner.arrival_by_id.contains_key(&p.id) && seen.insert(p.id))
        {
            return Err(PinError::Unknown);
        }
        Ok(())
    }

    /// Releases every pin of `pins`, or none: all are validated first.
    pub fn release_all(&self, pins: &[&PinId]) -> Result<(), PinError> {
        if pins.iter().any(|p| p.registry != self.registry) {
            return Err(PinError::Foreign);
        }
        let mut inner = self.lock();
        let live = |id: u64| inner.arrival_by_id.contains_key(&id);
        let mut seen = BTreeSet::new();
        if !pins.iter().all(|p| live(p.id) && seen.insert(p.id)) {
            return Err(PinError::Unknown);
        }
        for pin in pins {
            if let Some(arrival) = inner.arrival_by_id.remove(&pin.id) {
                inner.by_arrival.remove(&(arrival, pin.id));
            }
        }
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

    /// True while another holder has the registry lock (the lock is only held
    /// by the caller's own thread inside a registry method, so a test clock
    /// reading this learns whether it runs under the lock).
    #[cfg(test)]
    pub(crate) fn lock_is_held_elsewhere(&self) -> bool {
        matches!(
            self.inner.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }

    #[cfg(test)]
    pub(crate) fn with_next_id(next_id: u64) -> Self {
        Self::with_inner(
            Inner {
                next_id,
                ..Inner::default()
            },
            system_clock(),
        )
    }
}

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
