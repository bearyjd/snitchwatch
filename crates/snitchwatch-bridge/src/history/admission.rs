//! D5 admission, the real flood protection: a runaway or hostile process must
//! not be able to evict other applications' history.
//!
//! Three gates, checked in this order, all of which must pass before any of
//! them is charged (so a process refused by its own limit never spends the
//! global budget it was not given):
//!
//! 1. a per-process token bucket (default 20 events/s, burst 100);
//! 2. a per-process share of **retained bytes** (default 25% of the main
//!    file; bytes, not rows, so large records cannot dodge it);
//! 3. a global token bucket (default 200 events/s, burst 1,000).
//!
//! The bookkeeping is itself bounded (D5): at most [`MAX_BUCKETS`] token
//! buckets, idle ones dropped after [`IDLE_MS`], and a byte ledger of at most
//! the same size that, when full, drops its smallest entry (a flood of small
//! new paths cannot push out a large one). Processes are keyed by a keyed
//! hash of their path, never by the path itself, so memory does not depend on
//! path length and nobody can craft colliding keys.
//!
//! The clock is an argument. A clock that steps back credits no tokens, does
//! not panic, and does not stall admission: refilling resumes from the new
//! time.
//!
//! The byte ledger counts the event bytes each process holds in the store:
//! charged here, refunded by capture when a batch is lost, seeded from the
//! database at start and shrunk by [`Admission::release_bytes`] as retention
//! deletes rows. **It is not a gate**: the per-process byte share is enforced
//! by self-eviction (`retention.rs`, owner decision 2026-11-24, item 1), so a
//! process is never refused for the bytes it holds, only for its rates.
//! Refusals are returned to the caller, which counts them (in memory; the
//! aggregated gap row is PR 7).

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};

pub const GLOBAL_RATE: u64 = 200;
pub const GLOBAL_BURST: u64 = 1000;
pub const PROCESS_RATE: u64 = 20;
pub const PROCESS_BURST: u64 = 100;
pub const SHARE_PERCENT: u64 = 25;
pub const MAX_BUCKETS: usize = 1024;
pub const IDLE_MS: i64 = 60_000;

/// One token, in the thousandths the buckets count in (a rate of `r` events
/// a second refills `r` of these per millisecond).
const TOKEN: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    ProcessRate,
    GlobalRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionConfig {
    pub global_rate: u64,
    pub global_burst: u64,
    pub process_rate: u64,
    pub process_burst: u64,
    pub max_buckets: usize,
    pub idle_ms: i64,
}

impl AdmissionConfig {
    pub fn standard() -> Self {
        Self {
            global_rate: GLOBAL_RATE,
            global_burst: GLOBAL_BURST,
            process_rate: PROCESS_RATE,
            process_burst: PROCESS_BURST,
            max_buckets: MAX_BUCKETS,
            idle_ms: IDLE_MS,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    milli: u64,
    refilled_ms: i64,
    seen_ms: i64,
}

impl Bucket {
    fn full(burst: u64, now_ms: i64) -> Self {
        Self {
            milli: burst.saturating_mul(TOKEN),
            refilled_ms: now_ms,
            seen_ms: now_ms,
        }
    }

    /// Tokens as of `now_ms`. Time going backwards (a wall clock set back)
    /// adds nothing, and refilling resumes from the new time: nothing is
    /// credited twice, and a bucket is not stalled until the old time comes
    /// round again.
    fn refill(&mut self, rate: u64, burst: u64, now_ms: i64) {
        self.seen_ms = now_ms;
        if now_ms > self.refilled_ms {
            let elapsed = (now_ms - self.refilled_ms) as u64;
            self.milli = self
                .milli
                .saturating_add(elapsed.saturating_mul(rate))
                .min(burst.saturating_mul(TOKEN));
        }
        self.refilled_ms = now_ms;
    }

    fn has_token(&self) -> bool {
        self.milli >= TOKEN
    }

    fn take(&mut self) {
        self.milli -= TOKEN;
    }
}

pub struct Admission {
    config: AdmissionConfig,
    keys: RandomState,
    global: Bucket,
    buckets: HashMap<u64, Bucket>,
    bytes: HashMap<u64, u64>,
}

impl Admission {
    pub fn new(config: AdmissionConfig) -> Self {
        Self {
            global: Bucket::full(config.global_burst, 0),
            config,
            keys: RandomState::new(),
            buckets: HashMap::new(),
            bytes: HashMap::new(),
        }
    }

    fn key(&self, process: Option<&str>) -> u64 {
        self.keys.hash_one(process.unwrap_or(""))
    }

    /// Whether a record of `bytes` from `process` may be recorded at `now_ms`.
    /// On `Ok` the three gates are charged; on `Err` nothing is.
    pub fn admit(&mut self, process: Option<&str>, bytes: u64, now_ms: i64) -> Result<(), Refusal> {
        let key = self.key(process);
        if !self.buckets.contains_key(&key) {
            self.make_bucket_room(now_ms);
            self.buckets
                .insert(key, Bucket::full(self.config.process_burst, now_ms));
        }
        let (rate, burst) = (self.config.process_rate, self.config.process_burst);
        let bucket = self.buckets.get_mut(&key).expect("inserted above");
        bucket.refill(rate, burst, now_ms);
        if !bucket.has_token() {
            return Err(Refusal::ProcessRate);
        }
        self.global
            .refill(self.config.global_rate, self.config.global_burst, now_ms);
        if !self.global.has_token() {
            return Err(Refusal::GlobalRate);
        }
        self.global.take();
        self.buckets.get_mut(&key).expect("present").take();
        self.charge(key, bytes);
        Ok(())
    }

    fn charge(&mut self, key: u64, bytes: u64) {
        if !self.bytes.contains_key(&key) && self.bytes.len() >= self.config.max_buckets {
            if let Some(smallest) = self.bytes.iter().min_by_key(|(_, b)| **b).map(|(k, _)| *k) {
                self.bytes.remove(&smallest);
            }
        }
        let entry = self.bytes.entry(key).or_insert(0);
        *entry = entry.saturating_add(bytes);
    }

    /// Room for one more bucket: idle ones go first; when all are active the
    /// least recently seen goes. Never grows past the cap.
    fn make_bucket_room(&mut self, now_ms: i64) {
        if self.buckets.len() < self.config.max_buckets {
            return;
        }
        let idle = self.config.idle_ms;
        self.buckets.retain(|_, b| now_ms - b.seen_ms <= idle);
        if self.buckets.len() < self.config.max_buckets {
            return;
        }
        if let Some(oldest) = self
            .buckets
            .iter()
            .min_by_key(|(_, b)| b.seen_ms)
            .map(|(k, _)| *k)
        {
            self.buckets.remove(&oldest);
        }
    }

    /// Retention deleted `bytes` of `process`'s rows: its share frees up.
    /// Sets what `process` holds in the store (the database at start).
    pub fn seed_bytes(&mut self, process: Option<&str>, bytes: u64) {
        let key = self.key(process);
        self.bytes.remove(&key);
        if bytes > 0 {
            self.charge(key, bytes);
        }
    }

    pub fn release_bytes(&mut self, process: Option<&str>, bytes: u64) {
        let key = self.key(process);
        if let Some(held) = self.bytes.get_mut(&key) {
            *held = held.saturating_sub(bytes);
            if *held == 0 {
                self.bytes.remove(&key);
            }
        }
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    pub fn byte_entries(&self) -> usize {
        self.bytes.len()
    }

    /// Bytes admitted for `process` and not released.
    pub fn retained_for(&self, process: Option<&str>) -> u64 {
        self.bytes.get(&self.key(process)).copied().unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
