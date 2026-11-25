//! What the control gate asks of a [`Capture`]: stop accepting, advance the
//! generation under the acceptance lock, and stop the writer. A child of
//! [`super`] only to reach `Shared`'s private state.

use std::sync::Arc;

use super::super::budget::Budget;
use super::super::control_lock::WriterLock;
use super::super::lifecycle::{Exhausted, Generation};
use super::super::pins::PinRegistry;
use super::super::reader_registry::ControlGuard;
use super::super::writer::Engine;
use super::super::{load_state, locate, open_store, Mode, State};
use super::{Capture, CaptureConfig, CaptureStats};
use std::path::Path;

impl Capture {
    /// Step 2 of the barrier, brief and in memory: advance the generation
    /// (the registry is quiesced, so no reader can tag old data with the new
    /// number) and clear the consent, so no prompt or event is accepted from
    /// now on. Nothing after this can be refused: the writer stops next, so
    /// a prompt pending here is covered by a gap or discarded with its store,
    /// never left holding its pin for a capture that goes on.
    pub(in crate::history) fn close_admission(
        &self,
        guard: &ControlGuard,
        generation: &Generation,
    ) -> Result<(), Exhausted> {
        let mut control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.advance(generation)?;
        *control = None;
        Ok(())
    }

    /// This capture shares the writer lock, so the lock outlives the gate
    /// for as long as this writer can write.
    pub(in crate::history) fn holding(mut self, lock: Option<Arc<WriterLock>>) -> Self {
        self._lock = lock;
        self
    }

    /// Closes the queue and joins the writer, which drains what is queued
    /// and ends its session. `disabling` ends it as `disabled` and covers
    /// the prompts still pending with one `disabled` gap. Takes `&self`:
    /// the lock on `control` is not held across the join.
    pub(in crate::history) fn quiesce_writer(&self, disabling: bool) -> CaptureStats {
        self.shared
            .disabling
            .store(disabling, std::sync::atomic::Ordering::Release);
        self.stop();
        self.shared.stats()
    }
}

/// The bridge's one entry point. Reads the history state (which creates
/// nothing) and returns a running capture **only if history is enabled**;
/// off, unavailable, or a state directory the mode refuses gives `None`: no
/// thread, no directory, no queue.
pub fn start(mode: Mode, state_dir: Option<&Path>) -> Option<Arc<Capture>> {
    let location = locate(mode, state_dir).ok()?;
    // The lock first: only its holder may finish another run's removal.
    let lock = match super::super::control::lock_and_recover(&location) {
        Ok(lock) => lock,
        Err(reason) => {
            tracing::warn!(%reason, "history is held by another bridge");
            return None;
        }
    };
    let State::Enabled(settings) = load_state(&location) else {
        return None;
    };
    let budget = Budget::new(u64::from(settings.budget_mib) * 1024 * 1024).ok()?;
    let store = match open_store(&location) {
        Ok(store) => store,
        Err(reason) => {
            tracing::warn!(%reason, "history is enabled but could not be opened");
            return None;
        }
    };
    let engine = match Engine::new(store, &location, budget) {
        Ok(engine) => engine,
        Err(e) => {
            tracing::warn!(error = %e, "history is enabled but its writer could not start");
            return None;
        }
    };
    let capture = Capture::spawn(
        engine,
        Arc::new(PinRegistry::new()),
        CaptureConfig::for_settings(&budget, &settings),
    );
    match capture {
        Ok(capture) => Some(Arc::new(capture.holding(lock))),
        Err(e) => {
            tracing::warn!(error = %e, "history writer thread could not start");
            None
        }
    }
}

#[cfg(test)]
impl Capture {
    /// Stops the writer without touching consent or the generation.
    pub(in crate::history) fn quiesce_writer_for_test(&self) {
        self.stop();
    }

    /// Pins this capture's registry holds.
    pub(in crate::history) fn pins_for_test(&self) -> usize {
        self.shared.registry.len()
    }
}
