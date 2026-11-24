//! The capture pipeline's in-memory event counters ([`CaptureStats`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Events, not records: each count is the sum of `repeat_count`s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub seen: u64,
    pub skipped_nolog: u64,
    pub refused_process_rate: u64,
    pub refused_global_rate: u64,
    pub dropped_queue: u64,
    pub dropped_pin: u64,
    pub written: u64,
    pub lost_write: u64,
    /// Matched-rule events whose rule has no identity (recorded without a
    /// hash).
    pub unidentified_rules: u64,
    /// Rows stored with a rule hash but no snapshot (it could not be built).
    pub snapshot_errors: u64,
    /// Events lost to the pause: eviction could not make room (the `cap`
    /// of `write-failed (cap)`; the gap row itself is a plain `write-failed`).
    pub lost_cap: u64,
    /// Recording is paused for lack of room: the last attempt failed for it
    /// and none has succeeded since. Live state; not stored.
    pub cap_paused: bool,
    /// Age-expiry passes skipped because the wall clock had jumped.
    pub expiry_skipped: u64,
}

#[derive(Default)]
pub(super) struct Counters {
    pub(super) seen: AtomicU64,
    pub(super) skipped_nolog: AtomicU64,
    pub(super) refused_process_rate: AtomicU64,
    pub(super) refused_global_rate: AtomicU64,
    pub(super) dropped_queue: AtomicU64,
    pub(super) dropped_pin: AtomicU64,
    pub(super) written: AtomicU64,
    pub(super) lost_write: AtomicU64,
    pub(super) unidentified_rules: AtomicU64,
    pub(super) snapshot_errors: AtomicU64,
    pub(super) lost_cap: AtomicU64,
    pub(super) cap_paused: AtomicBool,
    pub(super) expiry_skipped: AtomicU64,
}

pub(super) fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

impl Counters {
    pub(super) fn snapshot(&self) -> CaptureStats {
        let c = self;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        CaptureStats {
            seen: get(&c.seen),
            skipped_nolog: get(&c.skipped_nolog),
            refused_process_rate: get(&c.refused_process_rate),
            refused_global_rate: get(&c.refused_global_rate),
            dropped_queue: get(&c.dropped_queue),
            dropped_pin: get(&c.dropped_pin),
            written: get(&c.written),
            lost_write: get(&c.lost_write),
            unidentified_rules: get(&c.unidentified_rules),
            snapshot_errors: get(&c.snapshot_errors),
            lost_cap: get(&c.lost_cap),
            cap_paused: c.cap_paused.load(Ordering::Relaxed),
            expiry_skipped: get(&c.expiry_skipped),
        }
    }
}
