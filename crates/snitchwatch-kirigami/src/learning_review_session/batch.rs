//! Running a frozen plan, and the rules that gate what touches the record.
//! No Qt: the controller supplies a sink (the bridge, or a double in tests).

use std::time::Duration;

use snitchwatch_bridge::ws_messages::ServerMessage;

use crate::bulk_rules::{
    run, BatchState, BulkPlan, Incoming, ListView, Listing, Progress, RuleSink, Settings,
};
use crate::rules::row_store::Rule;
use crate::settings_learning::SettingsStore;

/// What a finished batch left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    pub progress: Progress,
    /// The recorded data was deleted afterwards (confirmed by the writer).
    pub deleted: bool,
    /// Why it was not, when it should have been.
    pub delete_error: Option<String>,
}

/// Why the recorded data may not be deleted now, or `None` when it may.
/// Learning keeps writing it, and a batch reads it.
pub fn clear_refusal(learning_active: bool, applying: bool) -> Option<&'static str> {
    if learning_active {
        Some("Learning is running, so the recorded list is kept. Stop learning first.")
    } else if applying {
        Some("Rules are being applied, so the recorded list is kept until that ends.")
    } else {
        None
    }
}

/// Whether the recorded data is deleted after a batch: only a batch that
/// ended in `Finished` with every rule settled (nothing refused, rejected,
/// unconfirmed or unsent, so the record is still there to try again), and
/// only while learning is not running.
pub fn delete_after(progress: &Progress, learning_active: bool) -> bool {
    let c = progress.counts();
    !learning_active
        && progress.state == BatchState::Finished
        && c.refused + c.rejected + c.unconfirmed + c.not_sent + c.waiting == 0
}

/// Run `plan`, then delete the record through `delete` if [`delete_after`]
/// says so. `learning_active` is read after the batch ends. Blocks: call it
/// from a worker thread.
pub fn run_batch(
    plan: &BulkPlan,
    sink: &mut dyn RuleSink,
    settings: &Settings,
    learning_active: &dyn Fn() -> bool,
    delete: &mut dyn FnMut() -> Result<(), String>,
    observer: &mut dyn FnMut(&Progress),
) -> BatchOutcome {
    let progress = run(plan, sink, settings, observer);
    let (deleted, delete_error) = if delete_after(&progress, learning_active()) {
        match delete() {
            Ok(()) => (true, None),
            Err(why) => (false, Some(why)),
        }
    } else {
        (false, None)
    };
    BatchOutcome {
        progress,
        deleted,
        delete_error,
    }
}

/// [`run_batch`], but a panic in it comes back as an `Err` instead of
/// unwinding, so the caller can always tell the UI the batch ended.
pub fn run_batch_guarded(
    plan: &BulkPlan,
    sink: &mut dyn RuleSink,
    settings: &Settings,
    learning_active: &dyn Fn() -> bool,
    delete: &mut dyn FnMut() -> Result<(), String>,
    observer: &mut dyn FnMut(&Progress),
) -> Result<BatchOutcome, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_batch(plan, sink, settings, learning_active, delete, observer)
    }))
    .map_err(|_| "The batch stopped unexpectedly. Some rules may have been added.".to_string())
}

/// Delete the record under the settings gate, re-checking that learning is
/// not running and no batch is: a window that starts in the gap between an
/// earlier check and the deletion is applied under the same gate, so it
/// cannot have its memory or file wiped. `clear` returns the writer's result;
/// it waits for the writer thread (bounded) WHILE the gate is held, so a Qt
/// thread applying a snapshot can wait that long.
pub fn delete_record(
    store: &SettingsStore,
    learning_active: &dyn Fn() -> bool,
    applying: &dyn Fn() -> bool,
    clear: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let _gate = store.gate();
    if let Some(why) = clear_refusal(learning_active(), applying()) {
        return Err(why.to_string());
    }
    clear()
}

/// The committed rules list, read through `sink`, or `None` when no read
/// list arrived within `wait` (the page then skips nothing as present; the
/// sequencer still skips an identical rule when applying).
pub fn read_rules(sink: &mut dyn RuleSink, wait: Duration) -> Option<Vec<Rule>> {
    let mut view = ListView::new();
    view.expect_fresh();
    sink.request_snapshot().ok()?;
    let start = sink.now();
    while sink.now().saturating_sub(start) < wait {
        match sink.next_message(Duration::from_millis(250)).ok()? {
            Incoming::Message(
                message @ (ServerMessage::SetRules { .. }
                | ServerMessage::UpdateRules { .. }
                | ServerMessage::RulesNotShown { .. }),
            ) => {
                view.absorb(&message);
                if view.listing() == Listing::Ready {
                    return Some(view.rules().to_vec());
                }
            }
            Incoming::Lagged => {
                view.expect_fresh();
                sink.request_snapshot().ok()?;
            }
            Incoming::Message(_) | Incoming::Idle => {}
        }
    }
    None
}
