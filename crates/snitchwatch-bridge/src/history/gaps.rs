//! Connection history PR 7 (design D6): the single driver that turns losses
//! and discontinuities into gap rows, and the standing labels that are not
//! gaps. [`super::ledger`] stays the storage layer.
//!
//! | Cause | Source | When it becomes a row |
//! |---|---|---|
//! | `queue-overflow`, `admission-drop`, `write-failed` | in-memory [`LossCounter`]s | heartbeat, disable, shutdown |
//! | `upstream-loss` | [`UpstreamLoss`] of each ping | the same |
//! | `daemon-disconnected` | the watchdog's down and up edges | the up edge (or shutdown) |
//! | `clock-jump` | [`ClockWatch`], once per heartbeat | the heartbeat that saw it |
//! | `evicted` | [`Engine::record_evicted`] (PR 8 calls it) | at once |
//!
//! W1 holds throughout: a counter's pin is released only by the transaction
//! that writes its gap row ([`Engine::flush_counter_at`]); a flush that fails
//! puts its tally back, pin and all.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::durable::DurableError;
use super::ledger::{Gap, GapCause, LedgerLimits, LossCounter};
use super::pins::PinRegistry;
use super::session::SessionEnd;
use super::writer::Engine;
use crate::cache::rule_hits::UpstreamLoss;

/// A wall clock that moves this far more than the monotonic clock did over
/// one heartbeat interval has jumped (owner decision 2026-11-24).
pub const CLOCK_JUMP_THRESHOLD_MS: i64 = 60_000;

/// How much slower than the monotonic clock the wall clock may run over one
/// heartbeat before it counts as stepped back: NTP slews at up to 0.05%, so a
/// second absorbs it.
pub const CLOCK_SLEW_TOLERANCE_MS: i64 = 1_000;

/// What history cannot observe, as standing notes on the history view (D6):
/// not gaps with bounds. Wire exposure is PR 11.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StandingLabels {
    /// "Not observable: `nolog` rules" (the daemon reported such traffic).
    pub nolog: bool,
    /// "Not observable: default-applied traffic": no fork marker was seen
    /// this session, so the daemon may be stock.
    pub stock_default_unobservable: bool,
    /// "Not observable: no GUI session": a prompt arrived with no GUI.
    pub no_gui_prompts: bool,
}

/// Events that were never recorded, one tally (so one pin) per cause.
pub(super) struct Losses {
    pub(super) queue: LossCounter,
    pub(super) admission: LossCounter,
    pub(super) write_failed: LossCounter,
    pub(super) upstream: LossCounter,
    pub(super) disconnect: LossCounter,
    /// When the daemon came back; `None` while it is still away.
    disconnect_end: Option<i64>,
}

/// The tallies a flush walks through.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Queue,
    Admission,
    WriteFailed,
    Upstream,
    Disconnect,
}

const EVERY_SLOT: [Slot; 5] = [
    Slot::Queue,
    Slot::Admission,
    Slot::WriteFailed,
    Slot::Upstream,
    Slot::Disconnect,
];

impl Losses {
    pub(super) fn new() -> Self {
        Self {
            queue: LossCounter::new(GapCause::QueueOverflow),
            admission: LossCounter::new(GapCause::AdmissionDrop),
            write_failed: LossCounter::new(GapCause::WriteFailed),
            upstream: LossCounter::bounded(GapCause::UpstreamLoss),
            disconnect: LossCounter::new(GapCause::DaemonDisconnected),
            disconnect_end: None,
        }
    }

    fn slot(&mut self, slot: Slot) -> &mut LossCounter {
        match slot {
            Slot::Queue => &mut self.queue,
            Slot::Admission => &mut self.admission,
            Slot::WriteFailed => &mut self.write_failed,
            Slot::Upstream => &mut self.upstream,
            Slot::Disconnect => &mut self.disconnect,
        }
    }

    /// The daemon went away: the gap opens now, holding a pin. A second
    /// edge before the first closed is one longer gap, never a hole.
    pub(super) fn daemon_down(&mut self, registry: &PinRegistry) {
        self.disconnect_end = None;
        let _ = self.disconnect.record(registry, 1);
    }

    /// The daemon is back at `at_ms`: the gap closes there.
    pub(super) fn daemon_up(&mut self, at_ms: i64) {
        if self.disconnect.is_pending() {
            self.disconnect_end = Some(at_ms);
        }
    }

    /// What one ping showed was never received: the lower bound of events,
    /// plus one for a restart, whose losses nothing can count.
    pub(super) fn upstream_loss(&mut self, registry: &PinRegistry, loss: UpstreamLoss) {
        let n = loss.missing.saturating_add(u64::from(loss.restarted));
        if n == 0 {
            return;
        }
        let _ = match loss.since_ms {
            Some(since_ms) => self.upstream.record_since(registry, n, since_ms),
            None => self.upstream.record(registry, n),
        };
    }

    /// The open daemon gap's end, if it closed; a heartbeat leaves it open
    /// otherwise.
    fn disconnect_closed_at(&self) -> Option<i64> {
        self.disconnect_end
    }
}

fn locked(losses: &Mutex<Losses>) -> std::sync::MutexGuard<'_, Losses> {
    losses.lock().unwrap_or_else(|e| e.into_inner())
}

/// Flushes one tally without holding the lock across the commit: the tally
/// is taken out, written with its pin in one transaction, and put back (with
/// anything counted meanwhile) when that failed.
fn flush_slot(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    slot: Slot,
    end_ms: i64,
) -> Result<(), DurableError> {
    let mut taken = locked(losses).slot(slot).take();
    let result = engine.flush_counter_at(&mut taken, registry, limits, end_ms);
    let mut guard = locked(losses);
    if taken.is_pending() {
        guard.slot(slot).absorb(registry, taken);
    } else if matches!(slot, Slot::Disconnect) && !guard.disconnect.is_pending() {
        guard.disconnect_end = None;
    }
    result.map(|_| ())
}

/// Flushes every counter that is due. The heartbeat leaves an open
/// daemon-disconnected gap alone; `closing` (shutdown, disable) flushes it
/// too. The first error is returned, after every counter was tried.
pub(super) fn flush_due(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    closing: bool,
) -> Result<(), DurableError> {
    let mut first = Ok(());
    for slot in EVERY_SLOT {
        let end_ms = match slot {
            Slot::Disconnect => match locked(losses).disconnect_closed_at() {
                Some(end) => end,
                None if closing => registry.now(),
                None => continue,
            },
            _ => registry.now(),
        };
        let done = flush_slot(engine, registry, limits, losses, slot, end_ms);
        first = first.and(done);
    }
    first
}

/// Watches the wall clock against the monotonic one, once per heartbeat.
#[derive(Debug, Default)]
pub struct ClockWatch {
    /// `(wall_ms, mono_ms)` at the last heartbeat.
    last: Option<(i64, i64)>,
    unrecorded: Option<Gap>,
}

impl ClockWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// One heartbeat at `wall_ms` on the wall clock and `mono_ms` on the
    /// monotonic one. A wall clock that stepped back (moved less than the
    /// monotonic one by more than [`CLOCK_SLEW_TOLERANCE_MS`]; a step smaller
    /// than the interval still moves forward), or moved more than
    /// [`CLOCK_JUMP_THRESHOLD_MS`] further than the monotonic one since the
    /// last heartbeat, has jumped: the interval between the two wall times is
    /// a `clock-jump` gap, in which ordering is unreliable.
    pub fn observe(&mut self, wall_ms: i64, mono_ms: i64) {
        let Some((last_wall, last_mono)) = self.last.replace((wall_ms, mono_ms)) else {
            return;
        };
        let wall_delta = wall_ms.saturating_sub(last_wall);
        let mono_delta = mono_ms.saturating_sub(last_mono).max(0);
        let drift = wall_delta.saturating_sub(mono_delta);
        if (-CLOCK_SLEW_TOLERANCE_MS..=CLOCK_JUMP_THRESHOLD_MS).contains(&drift) {
            return;
        }
        let (start_ms, end_ms) = (last_wall.min(wall_ms), last_wall.max(wall_ms));
        self.unrecorded = Some(match self.unrecorded {
            Some(kept) => Gap {
                start_ms: kept.start_ms.min(start_ms),
                end_ms: kept.end_ms.max(end_ms),
                count: kept.count.saturating_add(1),
                ..kept
            },
            None => Gap {
                start_ms,
                end_ms,
                start_exact: true,
                cause: GapCause::ClockJump,
                count: 1,
            },
        });
    }

    /// The jump seen and not yet [`recorded`](Self::recorded).
    pub fn unrecorded(&self) -> Option<Gap> {
        self.unrecorded
    }

    /// The jump is durable.
    pub fn recorded(&mut self) {
        self.unrecorded = None;
    }
}

/// One heartbeat: the clock check, every due counter, then the commit that
/// advances the watermark. Each is tried; the first error returns.
pub(super) fn beat(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    clock: &mut ClockWatch,
    mono_ms: i64,
) -> Result<(), DurableError> {
    clock.observe(registry.now(), mono_ms);
    let jump = match clock.unrecorded() {
        Some(gap) => engine
            .commit_gap(&gap, &[], registry, limits)
            .map(|_| clock.recorded()),
        None => Ok(()),
    };
    let flushed = flush_due(engine, registry, limits, losses, false);
    let beat = engine.heartbeat(registry).map(|_| ());
    jump.and(flushed).and(beat)
}

/// The writer's heartbeat schedule and clock watch.
pub(super) struct Pulse {
    every: Duration,
    last: Instant,
    started: Instant,
    clock: ClockWatch,
}

impl Pulse {
    pub(super) fn new(every: Duration) -> Self {
        let now = Instant::now();
        Self {
            every,
            last: now,
            started: now,
            clock: ClockWatch::new(),
        }
    }

    /// Whether a heartbeat is due; asking again starts the next interval.
    pub(super) fn due(&mut self) -> bool {
        let due = self.last.elapsed() >= self.every;
        if due {
            self.last = Instant::now();
        }
        due
    }

    pub(super) fn beat(
        &mut self,
        engine: &mut Engine,
        registry: &PinRegistry,
        limits: &LedgerLimits,
        losses: &Mutex<Losses>,
    ) -> Result<(), DurableError> {
        let mono_ms = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        beat(engine, registry, limits, losses, &mut self.clock, mono_ms)
    }
}

/// Shutdown or disable: flushes everything (an open daemon gap ends now),
/// then ends the session as `kind`. Both are tried; the first error returns.
pub(super) fn close(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    kind: SessionEnd,
) -> Result<(), DurableError> {
    let flushed = flush_due(engine, registry, limits, losses, true);
    let ended = engine.end_session(registry, kind);
    flushed.and(ended)
}

impl Engine {
    /// Retention removed `removed` events, the oldest remaining being at
    /// `earliest_remaining_ms`. PR 8's eviction calls this in the transaction
    /// that deletes them.
    pub fn record_evicted(
        &mut self,
        earliest_remaining_ms: i64,
        removed: u64,
        registry: &PinRegistry,
        limits: &LedgerLimits,
    ) -> Result<(), DurableError> {
        if removed == 0 {
            return Ok(());
        }
        // What was removed is older than everything left; where it began is
        // unknown, so the marker has no exact start.
        let gap = Gap {
            start_ms: 0,
            end_ms: earliest_remaining_ms,
            start_exact: false,
            cause: GapCause::Evicted,
            count: removed,
        };
        self.commit_gap(&gap, &[], registry, limits).map(|_| ())
    }
}

#[cfg(test)]
#[path = "gaps_tests.rs"]
mod tests;
