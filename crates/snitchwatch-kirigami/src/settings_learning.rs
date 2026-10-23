//! Everything that writes the settings file goes through one transaction here
//! (learning mode, phase L1), so the end of a learning window cannot be undone
//! by an unrelated write, and a settings file that cannot be written cannot
//! leave "Allow" applied.
//!
//! Persisted learning keeps `silent_mode = Allow` on disk, so ending it must
//! reach the disk. If that write fails, the in-process FORCED-MODE override
//! holds the mode that is meant to be in force:
//!   * the mode the OWNER chose, when they explicitly chose one (a failed
//!     "Deny" stays Deny, never the old mode);
//!   * otherwise the window's `previous_mode` (`None` is "unchosen").
//!
//! While it holds, Allow is NOT applied (an intended Allow is attempted on
//! disk but applied as Off), every transaction also re-applies the intended
//! state (`silent_mode` = the intent, `learning` = none) to the file, and it is
//! cleared ONLY by such a write succeeding. The override lives in its own small
//! lock, never held across I/O, so the Qt thread can read it (`honor`,
//! `honor_mode`) without waiting for a file write.
//!
//! Each transaction is numbered (`Applied::generation`, taken inside the lock),
//! and the Qt side applies a snapshot only if no newer one was applied already
//! (`claim`): queue order is not transaction order, and a stale snapshot must
//! not put a replaced mode (a live Allow) back on screen.
//!
//! KNOWN LIMIT (pinned by the `known_limit_*` test, so a change here must touch
//! it): the override lives in memory only. If the settings file can NEVER be
//! written, then after an explicit mode choice (say Deny) fails while the
//! window is still live on disk, a restart re-reads Allow plus the live window
//! and applies it, for as long as the window has left (up to 7 days). An
//! EXPIRED window is ended again at every startup and never applies Allow. It
//! needs a persistently failing settings write, and L1 has no control that
//! starts learning, so it is unreachable today. L2 to L4 must keep it in mind
//! (the plan's "Honest limits" gets a line in the L5 documentation phase).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::learning::{self, Clocks, Ticker};
#[cfg(test)]
use crate::settings::Learning;
use crate::settings::{read_settings, write_settings, Settings, SilentMode};

/// `None`: no override. `Some(mode)`: the intended `silent_mode` (itself
/// optional, because "unchosen" is a value).
type Intent = Option<Option<SilentMode>>;

/// What is applied in-process for an intended mode: never Allow.
fn applied(intent: Option<SilentMode>) -> Option<SilentMode> {
    match intent {
        Some(SilentMode::Allow) => Some(SilentMode::Off),
        other => other,
    }
}

/// What a transaction leaves to apply: the settings in force and the number of
/// the transaction that produced them (see [`SettingsStore::claim`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub settings: Settings,
    pub generation: u64,
}

/// See [`SettingsStore::toggle_outcome`].
#[derive(Debug)]
pub struct ToggleOutcome<'a> {
    pub rdap_enabled: bool,
    pub preferences: Option<&'a Settings>,
}

/// The settings write lock plus the forced-mode override.
pub struct SettingsStore {
    /// Held across every read-modify-write of the settings file: two quick
    /// changes each run on their own thread, and without it the second could
    /// read the file before the first wrote it and overwrite the first change.
    write: Mutex<()>,
    forced: Mutex<Intent>,
    /// Transactions committed so far (bumped inside `write`).
    generation: AtomicU64,
    /// The newest generation handed out for applying.
    claimed: AtomicU64,
    /// Whether the last write attempt worked, so a failing write is logged
    /// once per change of state instead of at every attempt.
    last_write_ok: AtomicBool,
}

impl SettingsStore {
    pub const fn new() -> Self {
        Self {
            write: Mutex::new(()),
            forced: Mutex::new(None),
            generation: AtomicU64::new(0),
            claimed: AtomicU64::new(0),
            last_write_ok: AtomicBool::new(true),
        }
    }

    fn intent(&self) -> Intent {
        *self.forced.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn set_intent(&self, intent: Intent) {
        *self.forced.lock().unwrap_or_else(|p| p.into_inner()) = intent;
    }

    /// True while the override holds: a restore (or an explicit mode choice)
    /// could not be written to the settings file and is not yet saved.
    pub fn write_failed(&self) -> bool {
        self.intent().is_some()
    }

    /// `settings` with the override applied on top (a no-op without one).
    pub fn honor(&self, settings: Settings) -> Settings {
        match self.intent() {
            Some(intent) => Settings {
                silent_mode: applied(intent),
                learning: None,
                ..settings
            },
            None => settings,
        }
    }

    /// Whether a worker thread holding the snapshot numbered `generation` may
    /// act on it at once (without waiting for the Qt thread): only if no newer
    /// transaction was committed meanwhile, so a late, stale snapshot cannot
    /// undo what a newer one did.
    pub fn is_latest(&self, generation: u64) -> bool {
        generation >= self.generation.load(Ordering::SeqCst)
    }

    /// Whether `applied` says the learning window is over and is the newest
    /// word on it: recording stops now.
    pub fn ends_learning(&self, applied: &Applied) -> bool {
        applied.settings.learning.is_none() && self.is_latest(applied.generation)
    }

    /// The learning window in force: the one in `settings`, unless the
    /// override holds (then none, because Allow is not applied and nothing is
    /// answered, so nothing must be recorded).
    #[cfg(test)]
    pub fn learning_in_force(&self, settings: &Settings) -> Option<Learning> {
        self.honor(settings.clone()).learning
    }

    /// The mode to push to the silent-answer state: the override's, if any.
    pub fn honor_mode(&self, requested: SilentMode) -> SilentMode {
        match self.intent() {
            Some(intent) => applied(intent).unwrap_or_default(),
            None => requested,
        }
    }

    /// Whether the snapshot numbered `generation` may be applied now: only if
    /// nothing newer was applied already, and never in demo mode (which does
    /// not use the real settings file). Call it on the thread that applies.
    pub fn claim(&self, generation: u64, demo: bool) -> bool {
        !demo && self.claimed.fetch_max(generation, Ordering::SeqCst) < generation
    }

    /// What the RDAP toggle's queued closure does with its snapshot: the RDAP
    /// value always (it is not part of the silent-mode ordering), the rest of
    /// the preferences only if the snapshot may be applied (`claim`).
    pub fn toggle_outcome<'a>(&self, applied: &'a Applied, demo: bool) -> ToggleOutcome<'a> {
        ToggleOutcome {
            rdap_enabled: applied.settings.rdap_enabled,
            preferences: self
                .claim(applied.generation, demo)
                .then_some(&applied.settings),
        }
    }

    /// One locked read-modify-write; returns the state to APPLY.
    ///
    /// Order: read; the owner's explicit mode choice (ends learning, becomes
    /// the intent); `change`; the end check (an expired window is ended and
    /// its `previous_mode` becomes the intent unless one is already set); the
    /// held intent is re-applied; written when something changed or an intent
    /// is held. A successful write clears the override, a failed one keeps (or
    /// sets) it. The result is what is on disk with the override on top.
    pub fn transact(
        &self,
        path: &Path,
        now_wall_ms: i64,
        explicit: Option<SilentMode>,
        change: impl FnOnce(&mut Settings),
    ) -> Applied {
        let _write = self.write.lock().unwrap_or_else(|p| p.into_inner());
        let original = read_settings(path);
        let mut intent = self.intent();
        let mut settings = original.clone();
        if let Some(mode) = explicit {
            // A manual change ends learning and keeps the owner's mode.
            settings = learning::end_keeping_mode(Settings {
                silent_mode: Some(mode),
                ..settings
            });
            intent = Some(Some(mode));
        }
        change(&mut settings);
        if let Some(window) = &settings.learning {
            if learning::is_expired(window, now_wall_ms) {
                intent = intent.or(Some(window.previous_mode));
                settings = learning::end(settings);
            }
        }
        if let Some(mode) = intent {
            settings = Settings {
                silent_mode: mode,
                learning: None,
                ..settings
            };
        }
        if settings != original || intent.is_some() {
            match write_settings(path, &settings) {
                Ok(()) => {
                    self.set_intent(None);
                    if !self.last_write_ok.swap(true, Ordering::SeqCst) {
                        tracing::info!("the settings file can be written again");
                    }
                }
                Err(e) => {
                    if self.last_write_ok.swap(false, Ordering::SeqCst) {
                        tracing::warn!(error = %e, "failed to persist preferences");
                    }
                    self.set_intent(intent);
                }
            }
        }
        // Re-read, so a failed write still reports the on-disk state, with
        // the override (if any) on top.
        Applied {
            settings: self.honor(read_settings(path)),
            generation: self.generation.fetch_add(1, Ordering::SeqCst) + 1,
        }
    }
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Startup: end an expired window BEFORE the saved mode is applied, and prime
/// the running-time ticker if it is not primed yet (`refreshPreferences` also
/// runs on every Diagnostics page load; re-priming would drop the running
/// time counted so far).
pub fn startup(
    store: &SettingsStore,
    path: &Path,
    clocks: &dyn Clocks,
    ticker: &Mutex<Ticker>,
) -> Applied {
    ticker
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .prime(clocks.monotonic_ms());
    store.transact(path, clocks.wall_ms(), None, |_| {})
}

/// The expiry timer (about once a minute): add the running time since the last
/// tick to a live window, save it, end it if it is over.
pub fn tick(
    store: &SettingsStore,
    path: &Path,
    clocks: &dyn Clocks,
    ticker: &Mutex<Ticker>,
) -> Applied {
    let delta = ticker
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .delta(clocks.monotonic_ms());
    store.transact(path, clocks.wall_ms(), None, |settings| {
        settings.learning = settings
            .learning
            .as_ref()
            .map(|window| learning::advance(window, delta));
    })
}

/// The owner pressed "Stop learning": the window is over now, like an expired
/// one, so a restore that cannot be written is held as the override (Allow is
/// never applied) and written by the next transaction that works.
pub fn stop(store: &SettingsStore, path: &Path, clocks: &dyn Clocks) -> Applied {
    let now = clocks.wall_ms();
    store.transact(path, now, None, |settings| {
        if let Some(window) = settings.learning.as_mut() {
            window.ends_ms = window.ends_ms.min(now);
        }
    })
}

#[cfg(test)]
#[path = "settings_learning/tests.rs"]
mod tests;
