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
//! | `evicted` | the eviction transaction itself (`retention.rs`) | the same transaction |
//!
//! W1 holds throughout: a counter's pin is released only by the transaction
//! that writes its gap row ([`Engine::flush_counter_at`]); a flush that fails
//! puts its tally back, pin and all.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::durable::DurableError;
use super::ledger::{Gap, GapCause, LedgerLimits, LossCounter};
use super::pins::{PinId, PinRegistry};
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
    /// "Not observable: `nolog` rules": always, while history is on. The
    /// daemon skips such traffic before it reaches its statistics, so no
    /// ping ever reports it.
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
    /// Prompts pending when history was switched off (PR 10).
    pub(super) disabled: LossCounter,
    pub(super) upstream: LossCounter,
    /// Upstream losses from the last ping to the outage's start; the outage
    /// itself is the daemon gap's, and the rest is `upstream`'s.
    upstream_pre: LossCounter,
    /// Where `upstream_pre` ends: when the daemon went away.
    pre_outage_end: Option<i64>,
    pub(super) disconnect: LossCounter,
    /// When the daemon came back; `None` while it is still away.
    disconnect_end: Option<i64>,
    /// The open daemon gap's tally is out of its slot, being flushed: a
    /// recovery now must still close it if the flush fails and it comes back.
    disconnect_flushing: bool,
    /// When the daemon last came back, until the next ping has used it: what
    /// that ping reports as lost began no earlier (the outage is its own gap).
    back_at: Option<i64>,
    /// When the outage that `back_at` closes began.
    down_at: Option<i64>,
    /// One per ping, keyed by the ping's own time and held until the ping
    /// that reports its losses after that time has its events queued, so that
    /// a crash's `bridge-down` gap begins no later than that `since_ms` (the
    /// pin of the loss itself arrives later). Normally one; more only while
    /// pings overlap. The third field names the ping that took it while that
    /// ping is between begin and finish: no other ping may take or release it
    /// then, for its events are not queued yet.
    guards: Vec<(i64, PinId, Option<u64>)>,
    /// The id the next ping takes, so a guard names its owner.
    next_ping: u64,
    /// The `since` of every ping between begin and finish (`i64::MAX` when it
    /// has none): guards at or above the lowest are still some ping's own.
    in_flight: Vec<i64>,
    /// The session is ending: a ping after this would pin again and make the
    /// end `unclean`, so it is ignored.
    closed: bool,
    /// Pings whose guard pin could not be taken (pin ids exhausted).
    unguarded_pings: u64,
    /// Test seam: runs once, inside the disconnect slot's flush, between
    /// reading its end and taking its tally.
    #[cfg(test)]
    pub(super) between_read_and_take: Option<Box<dyn FnOnce() + Send>>,
}

/// The tallies a flush walks through.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Queue,
    Admission,
    WriteFailed,
    Disabled,
    UpstreamPre,
    Upstream,
    Disconnect,
}

const EVERY_SLOT: [Slot; 7] = [
    Slot::Queue,
    Slot::Admission,
    Slot::WriteFailed,
    Slot::Disabled,
    Slot::UpstreamPre,
    Slot::Upstream,
    Slot::Disconnect,
];

impl Losses {
    pub(super) fn new() -> Self {
        Self {
            queue: LossCounter::new(GapCause::QueueOverflow),
            admission: LossCounter::new(GapCause::AdmissionDrop),
            write_failed: LossCounter::new(GapCause::WriteFailed),
            disabled: LossCounter::new(GapCause::Disabled),
            upstream: LossCounter::bounded(GapCause::UpstreamLoss),
            upstream_pre: LossCounter::bounded(GapCause::UpstreamLoss),
            pre_outage_end: None,
            disconnect: LossCounter::new(GapCause::DaemonDisconnected),
            disconnect_end: None,
            disconnect_flushing: false,
            back_at: None,
            down_at: None,
            guards: Vec::new(),
            next_ping: 0,
            in_flight: Vec::new(),
            closed: false,
            unguarded_pings: 0,
            #[cfg(test)]
            between_read_and_take: None,
        }
    }

    /// Lets the watermark pass the last ping: a clean stop has nothing left
    /// to cover. Called before the session ends.
    pub(super) fn release_ping_guard(&mut self, registry: &PinRegistry) {
        self.closed = true;
        for (_, pin, _) in self.guards.drain(..) {
            let _ = registry.release(&pin);
        }
    }

    /// Pings that went unguarded because no pin could be taken.
    #[cfg(test)]
    pub(super) fn unguarded_pings(&self) -> u64 {
        self.unguarded_pings
    }

    fn slot(&mut self, slot: Slot) -> &mut LossCounter {
        match slot {
            Slot::Queue => &mut self.queue,
            Slot::Admission => &mut self.admission,
            Slot::WriteFailed => &mut self.write_failed,
            Slot::Disabled => &mut self.disabled,
            Slot::UpstreamPre => &mut self.upstream_pre,
            Slot::Upstream => &mut self.upstream,
            Slot::Disconnect => &mut self.disconnect,
        }
    }

    /// The daemon went away: the gap opens now, holding a pin. A second
    /// edge before the first closed is one longer gap, never a hole.
    pub(super) fn daemon_down(&mut self, registry: &PinRegistry) {
        self.disconnect_end = None;
        let fresh = !self.disconnect.is_pending();
        let _ = self.disconnect.record(registry, 1);
        // A new outage after one that came back unpinged starts a fresh
        // record; a second edge before the first closed keeps the first.
        if self.down_at.is_none() || self.back_at.is_some() {
            // The pin's own reading, so the pre-outage row ends exactly where
            // the daemon row starts.
            let at = fresh.then(|| self.disconnect.first_ms()).flatten();
            self.down_at = Some(at.unwrap_or_else(|| registry.now()));
            self.back_at = None;
        }
    }

    /// The daemon is back at `at_ms`: the gap closes there. The first word
    /// stands: a ping that proved the daemon up closed it, and the watchdog's
    /// later poll does not move the end.
    pub(super) fn daemon_up(&mut self, at_ms: i64) {
        if self.disconnect.is_pending() || self.disconnect_flushing {
            self.disconnect_end.get_or_insert(at_ms);
            // Only an open outage has an end to remember: after a ping
            // closed it, the next ping must not find a stale one.
            if self.down_at.is_some() {
                self.back_at.get_or_insert(at_ms);
            }
        }
    }

    /// A ping reached the bridge: pin it first and take what it reports its
    /// losses after, but release nothing yet. The ping's events are not
    /// queued, so the previous ping's guard still covers them until
    /// [`finish_ping`](Self::finish_ping). Everything the ping needs from
    /// shared state is taken here, in one step, and carried through.
    pub(super) fn begin_ping(
        &mut self,
        registry: &PinRegistry,
        loss: UpstreamLoss,
        events: u64,
    ) -> PingBegun {
        let now = registry.now();
        let arrival = loss.at_ms.map_or(now, |at| at.min(now));
        let mut begun = PingBegun {
            loss,
            events,
            previous: None,
            outage: None,
            in_flight: None,
            seq: self.next_ping,
        };
        self.next_ping = self.next_ping.wrapping_add(1);
        if self.closed {
            return begun;
        }
        // The ping is proof the daemon is up, whatever the watchdog saw yet.
        if self.down_at.is_some() {
            self.daemon_up(arrival);
        }
        if self.back_at.is_some() {
            begun.outage = self.back_at.take().zip(self.down_at.take());
        }
        begun.previous = self.take_guard_for(loss.since_ms);
        let since = loss.since_ms.unwrap_or(i64::MAX);
        self.in_flight.push(since);
        begun.in_flight = Some(since);
        let key = loss.at_ms.unwrap_or(arrival);
        match registry.add(arrival) {
            Ok(pin) => self.guards.push((key, pin, Some(begun.seq))),
            Err(error) => {
                self.unguarded_pings = self.unguarded_pings.saturating_add(1);
                // Powers of two only: a stuck registry must not flood the log.
                if self.unguarded_pings.is_power_of_two() {
                    tracing::warn!(
                        %error,
                        unguarded = self.unguarded_pings,
                        "history could not pin a ping; a crash's gap may start late"
                    );
                }
            }
        }
        self.release_unneeded_guards(registry);
        begun
    }

    /// The guard of the ping `since_ms` names: the one at exactly that time,
    /// and no other, since a ping that has not begun yet may need any older
    /// one. A ping with no `since` takes the latest, unless others are in
    /// flight. What no ping takes is released by
    /// [`release_unneeded_guards`](Self::release_unneeded_guards).
    fn take_guard_for(&mut self, since_ms: Option<i64>) -> Option<PinId> {
        let position = match since_ms {
            Some(since) => self
                .guards
                .iter()
                .position(|(key, _, owner)| *key == since && owner.is_none()),
            None if self.in_flight.is_empty() => self
                .guards
                .iter()
                .enumerate()
                .max_by_key(|(_, (key, _, _))| *key)
                .map(|(i, _)| i),
            None => None,
        }?;
        Some(self.guards.remove(position).1)
    }

    /// Releases the guards no ping can still take: those below the newest
    /// one at or under the lowest `since` in flight (every ping begun later
    /// names a `since` at or above the pings before it).
    fn release_unneeded_guards(&mut self, registry: &PinRegistry) {
        let bound = self.in_flight.iter().copied().min().unwrap_or(i64::MAX);
        let Some(anchor) = self
            .guards
            .iter()
            .map(|(key, _, _)| *key)
            .filter(|key| *key <= bound)
            .max()
        else {
            return;
        };
        let (obsolete, kept): (Vec<_>, Vec<_>) = self
            .guards
            .drain(..)
            .partition(|(key, _, owner)| *key < anchor && owner.is_none());
        self.guards = kept;
        for (_, pin, _) in obsolete {
            let _ = registry.release(&pin);
        }
    }

    /// The ping's events are queued: what it showed was never received is
    /// now tallied, and the previous guard is released or becomes the
    /// tally's pin (the gap then starts where the previous ping was seen).
    pub(super) fn finish_ping(&mut self, registry: &PinRegistry, begun: PingBegun) {
        if let Some(since) = begun.in_flight {
            if let Some(i) = self.in_flight.iter().position(|s| *s == since) {
                self.in_flight.swap_remove(i);
            }
        }
        for (_, _, owner) in &mut self.guards {
            if *owner == Some(begun.seq) {
                *owner = None;
            }
        }
        self.tally_ping(registry, begun);
        self.release_unneeded_guards(registry);
    }

    fn tally_ping(&mut self, registry: &PinRegistry, begun: PingBegun) {
        let PingBegun {
            loss,
            mut previous,
            outage,
            ..
        } = begun;
        // Deliberate: a loss reported once the session is closing is dropped,
        // not recorded, so that the end stays clean.
        if self.closed {
            if let Some(pin) = previous {
                let _ = registry.release(&pin);
            }
            return;
        }
        let n = loss.missing.saturating_add(u64::from(loss.restarted));
        let mut floor = outage.map(|(up, _)| up);
        if let (Some(outage), true) = (outage, n > 0) {
            if self.record_around_outage(registry, &mut previous, loss.since_ms, n, outage) {
                return;
            }
            // No pin for the part before the outage: overlap rather than a hole.
            floor = None;
        }
        match (previous, n) {
            (Some(pin), 0) => {
                let _ = registry.release(&pin);
            }
            (Some(pin), n) => self.upstream.record_pinned(registry, n, pin, floor),
            (None, 0) => {}
            (None, n) => {
                let _ = match loss.since_ms {
                    Some(since_ms) => {
                        let since_ms = floor.map_or(since_ms, |at| since_ms.max(at));
                        self.upstream.record_since(registry, n, since_ms)
                    }
                    None => self.upstream.record(registry, n),
                };
            }
        }
    }

    /// [`begin_ping`](Self::begin_ping) and [`finish_ping`](Self::finish_ping)
    /// with nothing between them.
    #[cfg(test)]
    pub(super) fn upstream_loss(&mut self, registry: &PinRegistry, loss: UpstreamLoss) {
        let begun = self.begin_ping(registry, loss, 0);
        self.finish_ping(registry, begun);
    }

    /// Records a loss that straddles an outage as two rows, so that nothing
    /// from the last ping to this one is left bare: `[ping, down]` here and
    /// `[up, now]` in `upstream`. The count is split, not doubled (each row
    /// needs one). False when no pin could be had for the first part. Each
    /// part holds a pin at its own start, so one part's failed write cannot
    /// let the watermark pass the other's stretch.
    fn record_around_outage(
        &mut self,
        registry: &PinRegistry,
        previous: &mut Option<PinId>,
        since_ms: Option<i64>,
        n: u64,
        (up, down): (i64, i64),
    ) -> bool {
        let start = previous
            .as_ref()
            .and_then(|pin| registry.arrival_of(pin))
            .or(since_ms)
            .filter(|start| *start < down);
        let Some(start) = start else {
            return false;
        };
        let pin = match previous.take() {
            Some(pin) => pin,
            None => match registry.add(start) {
                Ok(pin) => pin,
                Err(_) => return false,
            },
        };
        self.pre_outage_end = Some(down);
        self.upstream_pre.record_pinned(registry, 1, pin, None);
        let rest = n.saturating_sub(1).max(1);
        match registry.add(up) {
            Ok(pin) => self.upstream.record_pinned(registry, rest, pin, Some(up)),
            Err(_) => {
                let _ = self.upstream.record_since(registry, rest, up);
            }
        }
        true
    }
}

/// What a ping took from the shared state when it arrived, carried to the
/// moment its events are queued.
pub(super) struct PingBegun {
    loss: UpstreamLoss,
    /// Events the ping carried: what a ping cancelled before it finished
    /// has certainly lost.
    events: u64,
    /// The guard of the ping this one reports its losses after.
    previous: Option<PinId>,
    /// `(up, down)` of an outage that ended since the previous ping.
    outage: Option<(i64, i64)>,
    /// What this ping registered in [`Losses::in_flight`], if it did.
    in_flight: Option<i64>,
    /// The id its own guard carries until [`Losses::finish_ping`].
    seq: u64,
}

impl PingBegun {
    /// This ping never finished (its handler was dropped): its events are
    /// lost, so at least that many are missing.
    pub(super) fn cancelled(self) -> Self {
        let missing = self.loss.missing.max(self.events);
        Self {
            loss: UpstreamLoss {
                missing,
                ..self.loss
            },
            ..self
        }
    }
}

fn locked(losses: &Mutex<Losses>) -> std::sync::MutexGuard<'_, Losses> {
    losses.lock().unwrap_or_else(|e| e.into_inner())
}

/// Takes `slot`'s tally out under the lock the caller holds.
fn take_slot(guard: &mut Losses, slot: Slot) -> LossCounter {
    let taken = guard.slot(slot).take();
    if matches!(slot, Slot::Disconnect) {
        guard.disconnect_flushing = taken.is_pending();
    }
    taken
}

/// Where `slot`'s gap ends if it is due now, `None` if it is not: an open
/// daemon gap waits for its recovery unless the session is `closing`.
fn due_end(guard: &Losses, registry: &PinRegistry, slot: Slot, closing: bool) -> Option<i64> {
    let now = registry.now();
    match slot {
        Slot::Disconnect => guard.disconnect_end.or(closing.then_some(now)),
        Slot::UpstreamPre => Some(guard.pre_outage_end.unwrap_or(now)),
        _ => Some(now),
    }
}

/// The tally of `slot` and where its gap ends, if it is due: both under the
/// one lock the caller holds.
fn take_due(
    guard: &mut Losses,
    registry: &PinRegistry,
    slot: Slot,
    closing: bool,
) -> Option<(LossCounter, i64)> {
    let end = due_end(guard, registry, slot, closing)?;
    #[cfg(test)]
    if matches!(slot, Slot::Disconnect) {
        if let Some(hook) = guard.between_read_and_take.take() {
            hook();
        }
    }
    Some((take_slot(guard, slot), end))
}

/// Flushes one tally without holding the lock across the commit: the tally
/// is taken out, written with its pin in one transaction, and put back (with
/// anything counted meanwhile) when that failed.
#[cfg(test)]
fn flush_slot(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    slot: Slot,
    end_ms: i64,
) -> Result<(), DurableError> {
    let taken = take_slot(&mut locked(losses), slot);
    flush_taken(engine, registry, limits, losses, (slot, taken), end_ms)
}

fn flush_taken(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    (slot, mut taken): (Slot, LossCounter),
    end_ms: i64,
) -> Result<(), DurableError> {
    let result = engine.flush_counter_at(&mut taken, registry, limits, end_ms);
    let mut guard = locked(losses);
    guard.disconnect_flushing = false;
    settle_taken(&mut guard, registry, slot, taken);
    result.map(|_| ())
}

/// After a flush, under the lock: a tally that was not written goes back to
/// its slot, and a daemon gap that was is no longer open or ended.
fn settle_taken(guard: &mut Losses, registry: &PinRegistry, slot: Slot, taken: LossCounter) {
    if taken.is_pending() {
        guard.slot(slot).absorb(registry, taken);
    } else if matches!(slot, Slot::Disconnect) && !guard.disconnect.is_pending() {
        guard.disconnect_end = None;
    }
}

/// The tallies of one outage, committed together (#432): its pre-outage and
/// post-outage upstream-loss rows and the daemon row. Written one by one, a
/// read between the commits could see two unpaired count-1 upstream rows for
/// one lost event.
const OUTAGE_GROUP: [Slot; 3] = [Slot::UpstreamPre, Slot::Upstream, Slot::Disconnect];

/// A tally taken out of its slot with the end its gap would be written at.
type TakenSlot = (Slot, LossCounter, i64);

/// Writes `group` in one transaction (see
/// [`Engine::flush_counters_at`]); when that failed, every tally goes back
/// (with anything counted meanwhile) as [`flush_taken`] does for one.
fn flush_group(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    mut group: Vec<TakenSlot>,
) -> Result<(), DurableError> {
    let result = {
        let mut parts: Vec<(&mut LossCounter, i64)> = group
            .iter_mut()
            .map(|(_, taken, end_ms)| (taken, *end_ms))
            .collect();
        engine.flush_counters_at(&mut parts, registry, limits)
    };
    let mut guard = locked(losses);
    guard.disconnect_flushing = false;
    for (slot, taken, _) in group {
        settle_taken(&mut guard, registry, slot, taken);
    }
    result.map(|_| ())
}

/// Flushes every counter that is due. The heartbeat leaves an open
/// daemon-disconnected gap alone; `closing` (shutdown, disable) flushes it
/// too. The first error is returned, after every counter was tried. Each
/// tally's end and the tally itself are read under one lock, so an outage
/// that begins meanwhile is a tally of its own. The outage's three tallies
/// are taken under one lock and written in one transaction; the daemon's is
/// among them only once its end is known.
pub(super) fn flush_due(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    closing: bool,
) -> Result<(), DurableError> {
    let mut first = Ok(());
    for slot in EVERY_SLOT {
        if matches!(slot, Slot::UpstreamPre | Slot::Upstream | Slot::Disconnect) {
            continue;
        }
        let due = take_due(&mut locked(losses), registry, slot, closing);
        let Some((taken, end_ms)) = due else { continue };
        let done = flush_taken(engine, registry, limits, losses, (slot, taken), end_ms);
        first = first.and(done);
    }
    let group: Vec<TakenSlot> = {
        let mut guard = locked(losses);
        OUTAGE_GROUP
            .into_iter()
            .filter_map(|slot| {
                let (taken, end_ms) = take_due(&mut guard, registry, slot, closing)?;
                Some((slot, taken, end_ms))
            })
            .collect()
    };
    if !group.is_empty() {
        first = first.and(flush_group(engine, registry, limits, losses, group));
    }
    first
}

/// Watches the wall clock against the monotonic one, once per heartbeat.
#[derive(Debug, Default)]
pub struct ClockWatch {
    /// `(wall_ms, mono_ms)` at the last heartbeat.
    last: Option<(i64, i64)>,
    unrecorded: Option<Gap>,
    /// Held at the start of `unrecorded` until its row is durable, so that no
    /// commit in between lets the watermark pass the jump.
    pin: Option<PinId>,
    /// A jump was seen and no expiry pass has asked since.
    jumped: bool,
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
        self.jumped = true;
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

    /// Whether a jump was seen since the last call (sticky until taken): age
    /// expiry skips the pass that follows one.
    pub fn take_jumped(&mut self) -> bool {
        std::mem::take(&mut self.jumped)
    }

    /// The jump seen and not yet [`recorded`](Self::recorded).
    pub fn unrecorded(&self) -> Option<Gap> {
        self.unrecorded
    }

    /// The jump is durable (the transaction released its pin).
    pub fn recorded(&mut self) {
        self.unrecorded = None;
        self.pin = None;
    }

    /// Pins the start of the jump seen and not yet recorded, keeping the
    /// older pin when it already lies at or before it.
    fn hold(&mut self, registry: &PinRegistry) {
        let Some(gap) = self.unrecorded else {
            return;
        };
        let held = self.pin.as_ref().and_then(|pin| registry.arrival_of(pin));
        if held.is_some_and(|at| at <= gap.start_ms) {
            return;
        }
        if let Ok(pin) = registry.add(gap.start_ms) {
            if let Some(old) = self.pin.replace(pin) {
                let _ = registry.release(&old);
            }
        }
    }
}

/// Writes the jump the clock watch saw, if it has not been written: the row
/// and the release of the pin that held the watermark are one transaction.
fn record_jump(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    clock: &mut ClockWatch,
) -> Result<(), DurableError> {
    let Some(gap) = clock.unrecorded() else {
        return Ok(());
    };
    clock.hold(registry);
    // A small step back keeps the whole stepped-over span, though it ends
    // after now: that is where ordering is unreliable. A step past the jump
    // threshold (or a clock that was far ahead) would store a far-future end
    // that later widens collapses and deletes sessions, so it ends at now.
    let now = registry.now();
    let end_ms = if gap.end_ms.saturating_sub(now) > CLOCK_JUMP_THRESHOLD_MS {
        now
    } else {
        gap.end_ms
    };
    let gap = Gap {
        start_ms: gap.start_ms.min(end_ms),
        end_ms,
        ..gap
    };
    let committed = {
        let pins: Vec<&PinId> = clock.pin.iter().collect();
        engine.commit_gap(&gap, &pins, registry, limits)
    };
    committed.map(|_| clock.recorded())
}

/// One heartbeat: the clock check, every due counter, then the commit that
/// advances the watermark. Each is tried; the first error returns. A jump
/// that could not be written holds the watermark (its pin) until it can.
pub(super) fn beat(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    clock: &mut ClockWatch,
    mono_ms: i64,
) -> Result<(), DurableError> {
    clock.observe(registry.now(), mono_ms);
    let jump = record_jump(engine, registry, limits, clock);
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
    /// `wall_ms` is the wall clock now: the baseline the first heartbeat is
    /// judged against, so a jump before it is seen.
    pub(super) fn new(every: Duration, wall_ms: i64) -> Self {
        let now = Instant::now();
        let mut clock = ClockWatch::new();
        clock.observe(wall_ms, 0);
        Self {
            every,
            last: now,
            started: now,
            clock,
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
        let mono_ms = self.mono_ms();
        beat(engine, registry, limits, losses, &mut self.clock, mono_ms)
    }

    pub(super) fn close(
        &mut self,
        engine: &mut Engine,
        registry: &PinRegistry,
        limits: &LedgerLimits,
        losses: &Mutex<Losses>,
        kind: SessionEnd,
    ) -> Result<(), DurableError> {
        // A jump after the last heartbeat is seen now, before the end is
        // written over it.
        self.clock.observe(registry.now(), self.mono_ms());
        close(engine, registry, limits, losses, &mut self.clock, kind)
    }

    /// See [`ClockWatch::take_jumped`].
    pub(super) fn take_jumped(&mut self) -> bool {
        self.clock.take_jumped()
    }

    fn mono_ms(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// Shutdown or disable: records any pending clock jump, flushes everything
/// (an open daemon gap ends now), then ends the session as `kind`. Each is
/// tried; the first error returns.
pub(super) fn close(
    engine: &mut Engine,
    registry: &PinRegistry,
    limits: &LedgerLimits,
    losses: &Mutex<Losses>,
    clock: &mut ClockWatch,
    kind: SessionEnd,
) -> Result<(), DurableError> {
    locked(losses).release_ping_guard(registry);
    // A jump that could not be written before is written now, or its pin
    // keeps the end `unclean`.
    let jump = record_jump(engine, registry, limits, clock);
    let flushed = flush_due(engine, registry, limits, losses, true);
    let ended = engine.end_session(registry, kind);
    jump.and(flushed).and(ended)
}

#[cfg(test)]
#[path = "gaps_repair_tests.rs"]
mod repair_tests;
#[cfg(test)]
#[path = "gaps_tests.rs"]
mod tests;
