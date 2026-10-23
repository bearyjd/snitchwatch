//! Learning mode's recorder (phase L2).
//!
//! While a learning window runs, every connection the GUI answers with an
//! Allow (silent mode, `crate::silent_mode`) is recorded here, after the
//! answer was sent. `aggregate` says what is kept and how it is bounded,
//! `shared` is the live recorder and its writer thread, `store` is the file.
//!
//! GUI-only and in memory until the settings controller attaches the file
//! (`attach`): nothing here touches the user's real config directory unless
//! that is called.

mod aggregate;
mod classify;
mod program;
mod shared;
mod store;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub use aggregate::*;
pub use classify::{path_flag, PathFlag};
pub use shared::{Shared, Status, SEEN_CAP};
pub use store::{Backend, FileBackend, Loaded, Notice};

use crate::bridge_runtime::PendingRow;
use crate::learning::{Clocks, SystemClocks};

/// How long a burst of recording waits before it is saved.
const DEBOUNCE: Duration = Duration::from_secs(2);

/// Whether a learning window is running: set from the settings in force
/// (`SettingsController`), false whenever the forced-mode override holds.
static LEARNING: AtomicBool = AtomicBool::new(false);

fn global() -> &'static Arc<Shared> {
    static GLOBAL: OnceLock<Arc<Shared>> = OnceLock::new();
    GLOBAL.get_or_init(|| Shared::new(DEBOUNCE))
}

/// Whether a learning window is running.
pub fn is_active() -> bool {
    LEARNING.load(Ordering::SeqCst)
}

/// Say whether a window is running. True only when one just began: memory was
/// reset (a new window counts afresh) and the stored list is to be merged in
/// (on the writer thread, so a restart in mid-window carries on). Ending one
/// asks for a save; neither waits for the disk.
pub fn set_active(on: bool) -> bool {
    let was = LEARNING.swap(on, Ordering::SeqCst);
    match (was, on) {
        (false, true) => {
            global().reset();
            global().request_load();
            true
        }
        (true, false) => {
            global().flush();
            false
        }
        _ => false,
    }
}

/// Keep the recorded list in `path` from now on (once; later calls do
/// nothing).
pub fn attach(path: &Path) {
    global().attach(Arc::new(FileBackend::new(path.to_path_buf())));
}

/// Delete a stored list that is more than 30 days old.
pub fn discard_stale(path: &Path) -> bool {
    store::discard_if_stale(path, SystemClocks.wall_ms())
}

/// Record a row that was answered with Allow, if a window is running. Counted
/// once per qualified row id.
pub fn record(qualified_id: &str, row: &PendingRow) {
    if is_active() {
        global().record(qualified_id, row, SystemClocks.wall_ms());
    }
}

/// The feed lost messages: some connections were not recorded.
pub fn note_lagged() {
    if is_active() {
        global().note_lagged();
    }
}

/// What the banner shows.
pub fn status() -> Status {
    global().status()
}

/// A copy of what is recorded.
pub fn snapshot() -> Aggregate {
    global().snapshot()
}

/// Forget everything recorded and delete the file.
pub fn clear() {
    global().clear();
}

/// Forget everything and delete the file, and say whether the file is gone
/// (waits for the writer, so call it off the Qt thread).
pub fn clear_and_wait(timeout: Duration) -> Result<(), Notice> {
    global().clear_and_wait(timeout)
}

/// Save what is unsaved and wait a little for it (Stop, quitting).
pub fn flush_and_wait(timeout: Duration) -> bool {
    global().flush_and_wait(timeout)
}

/// What is stored in `path`, after saving what is unsaved. The review reads
/// this and not the memory, which is empty after a restart (it is filled only
/// when a window starts) and would count a stored list twice if merged.
/// Call it off the Qt thread: it waits for the writer.
pub fn stored(path: &Path) -> Loaded {
    flush_and_wait(STORED_FLUSH_WAIT);
    FileBackend::new(path.to_path_buf()).load(SystemClocks.wall_ms())
}

/// How long [`stored`] waits for unsaved rows to be written.
const STORED_FLUSH_WAIT: Duration = Duration::from_secs(3);

/// One lock for every test that flips `LEARNING` or touches the process-wide
/// recorder (or the silent state it follows): they share state, so they run
/// one at a time.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The fixed sentence for the saved list's trouble, or empty: the file's own
/// notice, and a write that went ahead without the lock.
pub fn notice_text(status: &Status) -> String {
    let lock = status.lock_degraded.then_some(
        "File locking was not available, so another copy recording at the same time could overwrite some counts.",
    );
    [status.notice.map(Notice::text), lock]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "learned/facade_tests.rs"]
mod tests;
