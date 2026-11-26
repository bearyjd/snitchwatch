//! Flushing the tallies of [`Losses`] to gap rows: which are due, which are
//! held back, and the one transaction that writes an outage's rows.

use std::sync::Mutex;

use super::super::durable::DurableError;
use super::super::ledger::{LedgerLimits, LossCounter};
use super::super::pins::PinRegistry;
use super::super::writer::Engine;
use super::{locked, Losses, Slot, EVERY_SLOT};

impl Losses {
    /// Whether the upstream tallies hold the halves of an outage whose
    /// daemon row cannot be written yet (the daemon dropped again before the
    /// flush, and the tally is now an open outage): written without it, they
    /// would read as two unpaired rows, so they wait for it. An unrelated
    /// loss already counted in the same `upstream` tally cannot be separated
    /// from its half and waits too, its pin holding the watermark. Halves of
    /// an outage whose row committed, and tallies without halves, never wait.
    fn halves_await_daemon(&self, registry: &PinRegistry) -> bool {
        self.upstream_pre.is_pending()
            && self.halves_outage > self.outages_written
            && self.disconnect.is_pending()
            && due_end(self, registry, Slot::Disconnect, false).is_none()
    }
}

/// Takes `slot`'s tally out under the lock the caller holds.
fn take_slot(guard: &mut Losses, slot: Slot) -> LossCounter {
    let taken = guard.slot(slot).take();
    if matches!(slot, Slot::Disconnect) {
        guard.disconnect_flushing = taken.is_pending();
        guard.outages_flushing = guard.outages_begun;
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
pub(super) fn flush_slot(
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
    } else if matches!(slot, Slot::Disconnect) {
        guard.outages_written = guard.outages_flushing;
        if !guard.disconnect.is_pending() {
            guard.disconnect_end = None;
        }
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
///
/// "All or none" holds per flush, not per outage: a flush that runs between
/// `begin_ping` and `finish_ping` finds the daemon row due but the upstream
/// rows not yet tallied, and commits the daemon row alone (the upstream rows
/// follow in a later flush, the watermark held by their pins meanwhile). One
/// bad pin in the group fails the whole transaction, so it blocks all of its
/// rows until it is gone.
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
    if let Err(error) = &result {
        tracing::warn!(%error, "history outage rows were not written together; retrying");
    }
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
pub(in crate::history) fn flush_due(
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
        let held = !closing && guard.halves_await_daemon(registry);
        OUTAGE_GROUP
            .into_iter()
            .filter(|_| !held)
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
