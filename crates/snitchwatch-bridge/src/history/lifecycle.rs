//! Connection history PR 3a: database-free lifecycle primitives (design D6/D8,
//! G1, C1, retention preview). Nothing here is called from the running bridge
//! yet; history stays dormant.
//!
//! * [`Generation`]: bumped by each control operation (clear/disable).
//! * [`AcceptanceSeq`]: one number per accepted item, for the preview boundary.
//! * [`ReaderGate`]: non-blocking admission of at most N concurrent readers.
//!
//! Counters never wrap: at `u64::MAX` they return [`Exhausted`] and stay put.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// A counter reached `u64::MAX`. It is left unchanged; the caller must treat
/// the history as unavailable rather than reuse a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("counter exhausted")]
pub struct Exhausted;

fn bump(counter: &AtomicU64) -> Result<u64, Exhausted> {
    counter
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
        .map(|prev| prev + 1)
        .map_err(|_| Exhausted)
}

/// The generation (G1): items remember the value they were accepted under and
/// settle only while it is still current.
#[derive(Debug, Default)]
pub struct Generation(AtomicU64);

impl Generation {
    pub fn new(start: u64) -> Self {
        Self(AtomicU64::new(start))
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    /// Increments and returns the new generation.
    pub fn advance(&self) -> Result<u64, Exhausted> {
        bump(&self.0)
    }
}

/// The acceptance sequence: strictly increasing, first number is 1, so a
/// boundary of 0 means "nothing accepted yet".
#[derive(Debug, Default)]
pub struct AcceptanceSeq(AtomicU64);

impl AcceptanceSeq {
    pub fn new(start: u64) -> Self {
        Self(AtomicU64::new(start))
    }

    /// The highest number assigned so far (0 if none).
    pub fn highest(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    /// Assigns the next number; unique across threads.
    pub fn assign(&self) -> Result<u64, Exhausted> {
        bump(&self.0)
    }

    /// True when something was accepted after `boundary` was recorded.
    pub fn is_above(&self, boundary: u64) -> bool {
        self.highest() > boundary
    }
}

/// Refused: every reader slot is taken. Never queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("busy")]
pub struct Busy;

/// Bridge-wide admission gate for history queries. Cheap to clone; clones
/// share the same slots.
#[derive(Debug, Clone)]
pub struct ReaderGate {
    in_use: Arc<AtomicUsize>,
    limit: usize,
}

impl ReaderGate {
    pub fn new(limit: usize) -> Self {
        Self {
            in_use: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    /// Takes a slot immediately or refuses with [`Busy`].
    pub fn try_acquire(&self) -> Result<ReaderPermit, Busy> {
        self.in_use
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < self.limit).then_some(n + 1)
            })
            .map(|_| ReaderPermit {
                in_use: Arc::clone(&self.in_use),
            })
            .map_err(|_| Busy)
    }

    pub fn in_use(&self) -> usize {
        self.in_use.load(Ordering::SeqCst)
    }
}

/// A held slot. Released only when dropped (also on panic unwind).
#[derive(Debug)]
pub struct ReaderPermit {
    in_use: Arc<AtomicUsize>,
}

impl Drop for ReaderPermit {
    fn drop(&mut self) {
        self.in_use.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
