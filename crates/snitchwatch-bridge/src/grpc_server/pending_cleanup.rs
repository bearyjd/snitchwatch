//! The cleanup every pending `AskRule` holds: it releases the prompt slot
//! and withdraws the row on every exit (moved out of `grpc_server.rs`).

use super::*;

/// Future-drop cleanup also runs for tonic transport cancellation. A closed
/// receiver makes late verdicts fail even while asynchronous cleanup waits
/// for the cache mutex; a settled verdict is never removed.
pub(super) struct PendingCleanup {
    cache: Arc<Mutex<ConnectionCache>>,
    row_id: String,
    slot: crate::prompt_slot::PromptSlotHandle,
    ask_id: u64,
}

impl PendingCleanup {
    /// Marks the prompt as holding the slot. Every exit from here, a dropped
    /// future included, releases it in `drop`.
    pub(super) fn hold(service: &UiService, row_id: String, ask_id: u64, what: String) -> Self {
        service.prompt_slot.hold(&row_id, what);
        Self {
            cache: service.cache.clone(),
            row_id,
            slot: service.prompt_slot.clone(),
            ask_id,
        }
    }
}

impl Drop for PendingCleanup {
    fn drop(&mut self) {
        self.slot.release(&self.row_id, self.ask_id);
        if let Ok(mut cache) = self.cache.try_lock() {
            cache.cancel_pending(&self.row_id);
        } else {
            let cache = self.cache.clone();
            let row_id = self.row_id.clone();
            tokio::spawn(async move {
                cache.lock().await.cancel_pending(&row_id);
            });
        }
    }
}
