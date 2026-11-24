//! The capture writer thread's loop: commit pinned batches, beat, expire,
//! flush, and end the session. A child of [`super`] (it works on [`Shared`]'s
//! private state); split out only to keep `capture.rs` under its size limit.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::super::capture_stats::add;
use super::super::durable::{DurableError, PinnedBatch};
use super::super::gaps;
use super::super::ledger::LedgerLimits;
use super::super::measure::estimate_bytes;
use super::super::pinned_queue::Entry;
use super::super::pins::PinId;
use super::super::retention::{Pause, Schedule};
use super::super::session::SessionEnd;
use super::super::writer::{Engine, WriteError};
use super::Shared;

/// How long the writer waits on a lock held by another connection.
const WRITER_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
/// Wait before the one retry of a failed batch, doubling per lost batch.
const RETRY_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(2);
/// A failing writer logs at most this often.
const ERROR_LOG_EVERY: Duration = Duration::from_secs(30);
/// After close the writer drains for at most this long; the rest is lost.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

/// What the writer thread keeps between batches.
struct Loop {
    limits: LedgerLimits,
    pulse: gaps::Pulse,
    /// Wait before the one retry of a failed batch.
    backoff: Duration,
    last_logged: Option<Instant>,
    /// Set while eviction cannot make room (D5).
    pause: Pause,
    expiry: Schedule,
}

/// Eviction could not make room: the pause, not a transient write error.
fn is_cap(error: &DurableError) -> bool {
    matches!(error, DurableError::Write(WriteError::Cap))
}

impl Shared {
    /// The writer thread: commit pinned batches until the queue is closed and
    /// empty, or, once closed, until [`SHUTDOWN_DRAIN`] has passed; beat every
    /// [`CaptureConfig::heartbeat_every`]; expire aged events; then flush the
    /// loss tallies and end the session.
    pub(super) fn run_writer(&self, mut engine: Engine, rx: Receiver<Entry>, mut began: bool) {
        let rows = engine.budget().rows_per_txn as usize;
        // A short lock wait: a locked database costs a batch, not the thread.
        let _ = engine.connection().busy_timeout(WRITER_BUSY_TIMEOUT);
        let mut st = Loop {
            limits: LedgerLimits::from_budget(engine.budget()),
            pulse: gaps::Pulse::new(self.heartbeat_every, self.registry.now()),
            backoff: RETRY_BACKOFF_MIN,
            last_logged: None,
            pause: Pause::new(self.pause_min, self.pause_max),
            expiry: Schedule::new(self.expiry_every, Instant::now()),
        };
        self.seed_admission(&engine);
        let mut deadline: Option<Instant> = None;
        while let Some(batch) = self.queue.pop_batch(&rx, rows, Duration::from_millis(200)) {
            if self.queue.is_closed() {
                let limit = *deadline.get_or_insert_with(|| Instant::now() + SHUTDOWN_DRAIN);
                if Instant::now() >= limit {
                    self.abandon(batch, &rx);
                    break;
                }
            }
            if !began {
                if let Err(e) = engine.begin_session(&self.registry, &st.limits) {
                    // Nothing is committed (not even a heartbeat) over the
                    // previous session; what is queued is lost, loudly.
                    log_rate_limited(&mut st.last_logged, &e, batch_events(&batch));
                    self.lost_batch(batch);
                    continue;
                }
                began = true;
            }
            self.housekeeping(&mut engine, &mut st);
            if !batch.items.is_empty() {
                self.write(&mut engine, batch, &mut st);
            }
        }
        self.finish(&mut engine, &st.limits, began, &mut st.pulse);
    }

    /// Admission's ledger starts from what the store holds, not from zero.
    fn seed_admission(&self, engine: &Engine) {
        match engine.event_bytes() {
            Ok(held) => {
                let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
                for (process, bytes) in held {
                    admission.seed_bytes(Some(&process), bytes);
                }
            }
            Err(e) => tracing::warn!(error = %e, "history could not read what it already holds"),
        }
    }

    /// Between batches: the heartbeat, an age-expiry pass when one is due,
    /// and giving admission back the bytes eviction removed.
    fn housekeeping(&self, engine: &mut Engine, st: &mut Loop) {
        if st.pulse.due() {
            let beat = st.pulse.beat(engine, &self.registry, &st.limits, &self.losses);
            if let Err(e) = beat {
                log_rate_limited(&mut st.last_logged, &e, 0);
            }
        }
        if st.expiry.due(Instant::now()) {
            let jumped = st.pulse.take_jumped();
            match engine.expire_aged(self.registry.now(), jumped) {
                Ok(pass) if pass.skipped_for_jump => add(&self.counters.expiry_skipped, 1),
                Ok(_) => {}
                Err(e) => log_rate_limited(&mut st.last_logged, &e, 0),
            }
        }
        let released = engine.take_released();
        if !released.is_empty() {
            let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
            for (process, bytes) in released {
                admission.release_bytes(Some(&process), bytes);
            }
        }
    }

    /// Writes one batch: one retry of a transient failure; recording pauses
    /// (with doubling backoff) when eviction cannot make room, and a batch
    /// that arrives during the pause is lost without another attempt.
    fn write(&self, engine: &mut Engine, batch: PinnedBatch, st: &mut Loop) {
        if st.pause.active(Instant::now()) {
            return self.lose_to_cap(batch);
        }
        let mut result = engine.commit_pinned(&batch, &self.registry);
        if result.as_ref().is_err_and(|e| !is_cap(e)) {
            // Once closed there is no time to wait: the retry is immediate.
            if !self.queue.is_closed() {
                std::thread::sleep(st.backoff);
            }
            result = engine.commit_pinned(&batch, &self.registry);
        }
        let events = batch_events(&batch);
        match result {
            Ok(_) => {
                add(&self.counters.written, events);
                st.backoff = RETRY_BACKOFF_MIN;
                if st.pause.reset() {
                    self.counters.cap_paused.store(false, Ordering::Relaxed);
                }
            }
            Err(e) if is_cap(&e) => {
                let delay = st.pause.enter(Instant::now());
                self.counters.cap_paused.store(true, Ordering::Relaxed);
                log_rate_limited(&mut st.last_logged, &format_args!("{e}; retrying in {delay:?}"), events);
                self.lose_to_cap(batch);
            }
            Err(e) => {
                self.lost_batch(batch);
                log_rate_limited(&mut st.last_logged, &e, events);
                st.backoff = (st.backoff * 2).min(RETRY_BACKOFF_MAX);
            }
        }
    }

    /// A batch lost to the pause: a plain `write-failed` tally, counted as
    /// the cap in live state (owner decision 2026-11-24, item 5).
    fn lose_to_cap(&self, batch: PinnedBatch) {
        add(&self.counters.lost_cap, batch_events(&batch));
        self.lost_batch(batch);
    }

    /// The queue is closed and drained: flush every tally (an open daemon
    /// gap ends now) and end the session. A failure leaves the pins held, so
    /// the next start still reconstructs a gap from the watermark.
    fn finish(
        &self,
        engine: &mut Engine,
        limits: &LedgerLimits,
        began: bool,
        pulse: &mut gaps::Pulse,
    ) {
        // Never began: ending would close the previous session as this one's.
        if !began && engine.begin_session(&self.registry, limits).is_err() {
            tracing::error!("history never started its session; the previous one stays open");
            return;
        }
        let kind = if self.disabling.load(Ordering::Acquire) {
            SessionEnd::Disabled
        } else {
            SessionEnd::Clean
        };
        if let Err(e) = pulse.close(engine, &self.registry, limits, &self.losses, kind) {
            tracing::error!(error = %e, "history shutdown could not record its gaps");
        }
    }

    /// The batch could not be written: count it, give its admission bytes
    /// back, and tally it as `write-failed` under its oldest pin, which holds
    /// the watermark until the tally is a gap row (W1).
    fn lost_batch(&self, batch: PinnedBatch) {
        let lost = batch_events(&batch);
        add(&self.counters.lost_write, lost);
        for item in &batch.items {
            self.refund(&item.event, estimate_bytes(&item.event));
        }
        if let Some(oldest) = self.release_all_but_oldest(batch.pins) {
            self.lose(|l| &mut l.write_failed, lost, Some(oldest));
        }
    }

    /// Shutdown ran out of time: everything still queued is lost.
    fn abandon(&self, batch: PinnedBatch, rx: &Receiver<Entry>) {
        let (mut items, mut pins) = (batch.items, batch.pins);
        for (item, pin) in rx.try_iter() {
            items.push(item);
            pins.push(pin);
        }
        self.lost_batch(PinnedBatch { items, pins });
    }

    /// Releases `pins` but the one that arrived first, which it returns.
    fn release_all_but_oldest(&self, mut pins: Vec<PinId>) -> Option<PinId> {
        let arrival = |p: &PinId| self.registry.arrival_of(p).unwrap_or(i64::MAX);
        let i = (0..pins.len()).min_by_key(|&i| arrival(&pins[i]))?;
        let oldest = pins.swap_remove(i);
        let refs: Vec<&PinId> = pins.iter().collect();
        let _ = self.registry.release_all(&refs);
        Some(oldest)
    }
}

/// A failing writer logs at most once per [`ERROR_LOG_EVERY`].
fn log_rate_limited(last: &mut Option<Instant>, error: &dyn std::fmt::Display, lost: u64) {
    if last.is_none_or(|t| t.elapsed() >= ERROR_LOG_EVERY) {
        *last = Some(Instant::now());
        tracing::error!(%error, lost, "history write failed");
    }
}

fn batch_events(batch: &PinnedBatch) -> u64 {
    batch
        .items
        .iter()
        .map(|i| u64::from(i.event.repeat_count))
        .sum()
}
