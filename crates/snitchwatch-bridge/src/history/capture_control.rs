//! What the control gate asks of a [`Capture`]: stop accepting, advance the
//! generation under the acceptance lock, and stop the writer. A child of
//! [`super`] only to reach `Shared`'s private state.

use std::sync::Arc;

use super::super::lifecycle::{Exhausted, Generation};
use super::super::reader_registry::ControlGuard;
use super::super::Consent;
use super::{Capture, CaptureStats};

impl Capture {
    /// Step 2 of the barrier, brief and in memory: advance the generation
    /// (the registry is quiesced, so no reader can tag old data with the new
    /// number) and clear the consent, so no prompt or event is accepted from
    /// now on. Returns the consent that was active, to put back if the
    /// operation is refused after this point.
    pub(in crate::history) fn close_admission(
        &self,
        guard: &ControlGuard,
        generation: &Generation,
    ) -> Result<Option<Arc<Consent>>, Exhausted> {
        let mut control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.advance(generation)?;
        Ok(control.take())
    }

    /// Undoes [`close_admission`](Self::close_admission)'s consent change.
    pub(in crate::history) fn restore_consent(&self, consent: Option<Arc<Consent>>) {
        *self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = consent;
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
