//! `SettingsController` — autostart toggle + crash-log surfacing (Tasks 15,
//! 16), bound by `DiagnosticsPage.qml`, plus the LS redesign's two choices:
//! the duration an Allow remembers (`defaultDuration`) and the silent mode
//! (`silentMode`; empty until chosen), read by `main.qml`, the alert and the
//! onboarding step.
//!
//! Both underlying operations are plain `std::fs` calls (`crate::autostart`,
//! `crate::crash_log`) with no bridge/network dependency, but disk I/O still
//! runs on a scratch thread and reports back via `CxxQtThread::queue` (the
//! same pattern `WizardController` uses for its `systemctl` calls) so a slow
//! or unavailable filesystem (e.g. a networked home directory) never blocks
//! the Qt thread. Every failure path sets `autostartError`/`crashLogText` to a
//! human-readable message rather than panicking.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

use crate::autostart::{read_autostart_state, remove_autostart_desktop, write_autostart_desktop};
use crate::coexistence::check_coexistence;
use crate::crash_log::read_crash_log_tail;
use crate::learning::{self, Clocks, SystemClocks, Ticker};
use crate::paths::{autostart_path, crash_log_path, learned_path, settings_path};
use crate::settings::{read_settings, Settings, SilentMode};
use crate::settings_learning::{self, Applied, SettingsStore};

const CRASH_LOG_TAIL_LINES: usize = 200;

/// The `Exec=` target written into the autostart `.desktop` file. Prefers the
/// absolute path to the currently-running binary (works regardless of
/// `$PATH`); falls back to the bare binary name (matches
/// `[[bin]] name` in Cargo.toml) if `current_exe()` fails, e.g. under an
/// unusual sandbox.
fn autostart_exec_target() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "snitchwatch-kirigami".to_string())
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Settings & Diagnostics surface (Tasks 15, 16).
        #[qobject]
        #[qml_element]
        /// Whether the XDG autostart `.desktop` file currently exists.
        #[qproperty(bool, autostart_enabled, cxx_name = "autostartEnabled")]
        /// Non-empty when the last autostart read/write failed.
        #[qproperty(QString, autostart_error, cxx_name = "autostartError")]
        /// The last 200 lines of `crash.log`, or a friendly placeholder.
        #[qproperty(QString, crash_log_text, cxx_name = "crashLogText")]
        /// Whether the pending-decision insight panel may query RDAP
        /// (`rdap.org`) for registration info. Opt-in, default off; reverse
        /// DNS is unaffected. See `crate::settings` and `PendingInsight`.
        #[qproperty(bool, rdap_enabled, cxx_name = "rdapEnabled")]
        /// The duration token an Allow remembers for (`this_time`,
        /// `for_5_minutes`, `until_restart`, `forever`). `forever` until
        /// the persisted value is read. See `crate::settings`.
        #[qproperty(QString, default_duration, cxx_name = "defaultDuration")]
        /// `off`, `allow` or `deny`; empty until the user has chosen (the
        /// onboarding step asks). Empty acts as `off`.
        #[qproperty(QString, silent_mode, cxx_name = "silentMode")]
        /// Whether the persisted settings have been read yet, so the
        /// onboarding step is not shown on a stale empty `silentMode`.
        #[qproperty(bool, preferences_loaded, cxx_name = "preferencesLoaded")]
        /// True while the settings file could not be written and the silent
        /// mode in force is an in-process override (never Allow); a later
        /// phase shows "the settings file could not be written".
        #[qproperty(bool, settings_write_failed, cxx_name = "settingsWriteFailed")]
        /// Whether a learning window is running (never while the forced-mode
        /// override holds). The banner shows only then.
        #[qproperty(bool, learning_active, cxx_name = "learningActive")]
        /// When the window ends, wall-clock milliseconds since the epoch (the
        /// nearer of its two bounds, as of the last change); 0 without one.
        #[qproperty(i64, learning_ends_ms, cxx_name = "learningEndsMs")]
        /// How many programs learning mode has recorded (polled by
        /// `refreshLearningStatus`).
        #[qproperty(i32, programs_seen, cxx_name = "programsSeen")]
        /// Some connections were missed (the feed lagged), so the record is
        /// not complete.
        #[qproperty(bool, learning_incomplete, cxx_name = "learningIncomplete")]
        /// Programs the record refused or dropped to stay within its caps.
        #[qproperty(i32, dropped_count, cxx_name = "droppedCount")]
        /// A fixed sentence about the saved list (damaged, not ours, could not
        /// be saved, no file lock), or empty.
        #[qproperty(QString, learned_notice, cxx_name = "learnedNotice")]
        /// Where the saved list lives.
        #[qproperty(QString, learned_path, cxx_name = "learnedPath")]
        /// A fixed sentence when "Start learning" did nothing, else empty.
        #[qproperty(QString, learning_start_error, cxx_name = "learningStartError")]
        /// True when upstream opensnitch-ui was detected (package installed
        /// or its autostart entry present) — see `crate::coexistence`.
        #[qproperty(bool, coexistence_conflict, cxx_name = "coexistenceConflict")]
        /// Human-readable detail for `coexistenceConflict`, always set
        /// (even when there's no conflict) so the page can show a positive
        /// "no conflict" confirmation too.
        #[qproperty(QString, coexistence_detail, cxx_name = "coexistenceDetail")]
        /// True while a background refresh/write is in flight.
        #[qproperty(bool, busy)]
        type SettingsController = super::SettingsControllerRust;

        /// Re-read the autostart file's presence off the UI thread. Called
        /// from `DiagnosticsPage.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "refreshAutostart"]
        fn refresh_autostart(self: Pin<&mut SettingsController>);

        /// Write or remove the autostart `.desktop` file, then refresh
        /// `autostartEnabled`/`autostartError` from the result. Wired to the
        /// Diagnostics page's autostart switch.
        #[qinvokable]
        #[cxx_name = "setAutostart"]
        fn set_autostart(self: Pin<&mut SettingsController>, enabled: bool);

        /// Re-read the last 200 lines of `crash.log` off the UI thread.
        /// Called on page load and from the "Refresh" button.
        #[qinvokable]
        #[cxx_name = "refreshCrashLog"]
        fn refresh_crash_log(self: Pin<&mut SettingsController>);

        /// Re-read the persisted RDAP opt-in setting off the UI thread.
        /// Called from `DiagnosticsPage.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "refreshRdapEnabled"]
        fn refresh_rdap_enabled(self: Pin<&mut SettingsController>);

        /// Persist the RDAP opt-in toggle, then refresh `rdapEnabled` from
        /// the result. Wired to the Diagnostics page's RDAP switch. Named
        /// `toggleRdapEnabled` (not `setRdapEnabled`) to avoid colliding
        /// with the `rdapEnabled` qproperty's own auto-generated setter.
        #[qinvokable]
        #[cxx_name = "toggleRdapEnabled"]
        fn toggle_rdap_enabled(self: Pin<&mut SettingsController>, enabled: bool);

        /// Read `defaultDuration` and `silentMode` from the settings file off
        /// the UI thread. Called from `main.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "refreshPreferences"]
        fn refresh_preferences(self: Pin<&mut SettingsController>);

        /// Persist the Allow duration (a known token; anything else is
        /// stored as `forever`), then refresh `defaultDuration` from disk.
        #[qinvokable]
        #[cxx_name = "chooseDefaultDuration"]
        fn choose_default_duration(self: Pin<&mut SettingsController>, token: &QString);

        /// Persist the silent mode (`off`, `allow`, `deny`; anything else is
        /// ignored), then refresh `silentMode` from disk.
        #[qinvokable]
        #[cxx_name = "chooseSilentMode"]
        fn choose_silent_mode(self: Pin<&mut SettingsController>, token: &QString);

        /// The learning window's expiry timer (`AlertHost.qml`, about once a
        /// minute): saves the running time of a live window and ends an
        /// expired one, restoring the mode it replaced. Writes nothing when no
        /// window is running.
        #[qinvokable]
        #[cxx_name = "tickLearning"]
        fn tick_learning(self: Pin<&mut SettingsController>);

        /// "Stop learning": ends the window now (restoring the mode it
        /// replaced) under the settings lock, then saves what was seen.
        #[qinvokable]
        #[cxx_name = "stopLearning"]
        fn stop_learning(self: Pin<&mut SettingsController>);

        /// "Start learning" for `hours` (above zero, at most 7 days): `allow`
        /// and a recording window, under the settings lock. Refused (see
        /// `learningStartError`) while one runs or the file cannot be written.
        #[qinvokable]
        #[cxx_name = "startLearning"]
        fn start_learning(self: Pin<&mut SettingsController>, hours: i32);

        /// Re-read the recorder's counters (cheap, no disk): the banner's
        /// timer calls it while a window runs.
        #[qinvokable]
        #[cxx_name = "refreshLearningStatus"]
        fn refresh_learning_status(self: Pin<&mut SettingsController>);

        /// Re-run the opensnitch-ui coexistence check off the UI thread.
        /// Called on page load and from the Diagnostics page's "Check again"
        /// button.
        #[qinvokable]
        #[cxx_name = "refreshCoexistence"]
        fn refresh_coexistence(self: Pin<&mut SettingsController>);
    }

    impl cxx_qt::Threading for SettingsController {}
}

/// Rust-side state for [`qobject::SettingsController`].
pub struct SettingsControllerRust {
    autostart_enabled: bool,
    autostart_error: QString,
    crash_log_text: QString,
    rdap_enabled: bool,
    default_duration: QString,
    silent_mode: QString,
    preferences_loaded: bool,
    settings_write_failed: bool,
    learning_active: bool,
    learning_ends_ms: i64,
    programs_seen: i32,
    learning_incomplete: bool,
    dropped_count: i32,
    learned_notice: QString,
    learned_path: QString,
    learning_start_error: QString,
    coexistence_conflict: bool,
    coexistence_detail: QString,
    busy: bool,
}

impl Default for SettingsControllerRust {
    fn default() -> Self {
        Self {
            autostart_enabled: false,
            autostart_error: QString::default(),
            crash_log_text: QString::from("Loading\u{2026}"),
            rdap_enabled: false,
            default_duration: QString::from(crate::settings::DEFAULT_DURATION),
            silent_mode: QString::default(),
            preferences_loaded: false,
            settings_write_failed: false,
            learning_active: false,
            learning_ends_ms: 0,
            programs_seen: 0,
            learning_incomplete: false,
            dropped_count: 0,
            learned_notice: QString::default(),
            learned_path: QString::from(&*learned_path().to_string_lossy()),
            learning_start_error: QString::default(),
            coexistence_conflict: false,
            coexistence_detail: QString::from("Checking\u{2026}"),
            busy: false,
        }
    }
}

impl qobject::SettingsController {
    fn refresh_autostart(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let state = read_autostart_state(&autostart_path());
            let _ = qt_thread.queue(move |qobject| {
                qobject.apply_autostart_result(state.enabled, None);
            });
        });
    }

    fn set_autostart(mut self: Pin<&mut Self>, enabled: bool) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let path = autostart_path();
            let outcome = if enabled {
                write_autostart_desktop(&path, &autostart_exec_target())
            } else {
                remove_autostart_desktop(&path)
            };
            let error = outcome.err().map(|e| e.to_string());
            // Re-read from disk rather than trusting `enabled` blindly, so a
            // failed write still reports the file's actual on-disk state.
            let state = read_autostart_state(&path);
            let _ = qt_thread.queue(move |qobject| {
                qobject.apply_autostart_result(state.enabled, error);
            });
        });
    }

    fn refresh_crash_log(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let text = read_crash_log_tail(&crash_log_path(), CRASH_LOG_TAIL_LINES);
            let _ = qt_thread.queue(move |mut qobject| {
                qobject.as_mut().set_crash_log_text(QString::from(&text));
                qobject.as_mut().set_busy(false);
            });
        });
    }

    fn refresh_rdap_enabled(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let enabled = read_settings(&settings_path()).rdap_enabled;
            let _ = qt_thread.queue(move |mut qobject| {
                qobject.as_mut().set_rdap_enabled(enabled);
                qobject.as_mut().set_busy(false);
            });
        });
    }

    fn toggle_rdap_enabled(mut self: Pin<&mut Self>, enabled: bool) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            // Preserves any other persisted fields rather than clobbering them
            // with a lone-field write, and ends an expired learning window like
            // every other write; the transaction reports the on-disk state.
            let actual = SETTINGS_WRITE.transact(
                &settings_path(),
                SystemClocks.wall_ms(),
                None,
                move |settings| settings.rdap_enabled = enabled,
            );
            push_forced_state(&actual);
            let _ = qt_thread.queue(move |mut qobject| {
                // The RDAP value is always shown; the other preferences only
                // if no newer snapshot was applied already (never in demo mode).
                let _gate = SETTINGS_WRITE.gate();
                let outcome = SETTINGS_WRITE.toggle_outcome(&actual, crate::demo::enabled());
                qobject.as_mut().set_rdap_enabled(outcome.rdap_enabled);
                if let Some(settings) = outcome.preferences {
                    qobject.as_mut().apply_preferences(settings);
                }
                qobject.as_mut().set_busy(false);
            });
        });
    }

    fn refresh_preferences(self: Pin<&mut Self>) {
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            // Demo mode (screenshots) must not depend on this machine's file.
            let applied = if crate::demo::enabled() {
                // No transaction in demo mode: applied as is.
                None
            } else {
                // An expired learning window is ended (and its mode restored)
                // BEFORE anything is applied: a saved Allow from a window that
                // ran out while the GUI was closed is never applied.
                // An old saved list is deleted, then the recorder is pointed at
                // the file (nothing is read or written before this).
                crate::learned::discard_stale(&learned_path());
                crate::learned::attach(&learned_path());
                let settings = settings_learning::startup(
                    &SETTINGS_WRITE,
                    &settings_path(),
                    &SystemClocks,
                    &TICKER,
                );
                push_forced_state(&settings);
                Some(settings)
            };
            let _ = qt_thread.queue(move |qobject| match applied {
                Some(applied) => qobject.apply_if_newest(&applied),
                None => qobject.apply_preferences(&Settings::default()),
            });
        });
    }

    fn tick_learning(self: Pin<&mut Self>) {
        if crate::demo::enabled() {
            return;
        }
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let applied =
                settings_learning::tick(&SETTINGS_WRITE, &settings_path(), &SystemClocks, &TICKER);
            push_forced_state(&applied);
            let _ = qt_thread.queue(move |qobject| qobject.apply_if_newest(&applied));
        });
    }

    fn stop_learning(self: Pin<&mut Self>) {
        if crate::demo::enabled() {
            return;
        }
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let applied = settings_learning::stop(&SETTINGS_WRITE, &settings_path(), &SystemClocks);
            push_forced_state(&applied);
            // Off the Qt thread, so waiting for the disk is fine here.
            if !crate::learned::flush_and_wait(LEARNED_FLUSH_WAIT) {
                tracing::warn!("learned: the saved list was not written in time");
            }
            let _ = qt_thread.queue(move |qobject| qobject.apply_if_newest(&applied));
        });
    }

    fn start_learning(mut self: Pin<&mut Self>, hours: i32) {
        if crate::demo::enabled() {
            return;
        }
        self.as_mut().set_learning_start_error(QString::default());
        // Rules are being applied from the record: not now.
        if crate::learning_review_controller::applying_now() {
            self.as_mut().set_learning_start_error(QString::from(
                "Rules are being applied from the recorded list; wait until that ends.",
            ));
            return;
        }
        let duration_ms = i64::from(hours).saturating_mul(3_600_000);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let started = settings_learning::start(
                &SETTINGS_WRITE,
                &settings_path(),
                &SystemClocks,
                duration_ms,
            );
            match started {
                Ok(applied) => {
                    push_forced_state(&applied);
                    let _ = qt_thread.queue(move |mut qobject| {
                        qobject
                            .as_mut()
                            .set_learning_start_error(QString::default());
                        qobject.apply_if_newest(&applied);
                    });
                }
                Err(refusal) => {
                    let _ = qt_thread.queue(move |mut qobject| {
                        qobject
                            .as_mut()
                            .set_learning_start_error(QString::from(refusal.text()));
                    });
                }
            }
        });
    }

    fn refresh_learning_status(mut self: Pin<&mut Self>) {
        let status = crate::learned::status();
        let count = |n: u64| i32::try_from(n).unwrap_or(i32::MAX);
        self.as_mut()
            .set_programs_seen(i32::try_from(status.programs_seen).unwrap_or(i32::MAX));
        self.as_mut().set_learning_incomplete(status.incomplete);
        self.as_mut().set_dropped_count(count(status.dropped));
        self.as_mut()
            .set_learned_notice(QString::from(&crate::learned::notice_text(&status)));
    }

    fn choose_default_duration(self: Pin<&mut Self>, token: &QString) {
        let token = crate::settings::normalize_duration(&token.to_string());
        self.persist_preferences(None, move |settings| {
            settings.default_duration = token.to_string();
        });
    }

    fn choose_silent_mode(self: Pin<&mut Self>, token: &QString) {
        // An unknown token must not become a silent Deny (or anything else).
        let Some(mode) = SilentMode::from_token(&token.to_string()) else {
            tracing::warn!("SettingsController: unknown silent mode ignored");
            return;
        };
        // A manual change ends any learning window and keeps this mode.
        self.persist_preferences(Some(mode), move |settings| {
            settings.silent_mode = Some(mode);
            settings.learning = None;
        });
    }

    fn refresh_coexistence(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let report = check_coexistence();
            let conflict = report.conflict();
            let detail = report.message();
            let _ = qt_thread.queue(move |mut qobject| {
                qobject.as_mut().set_coexistence_conflict(conflict);
                qobject
                    .as_mut()
                    .set_coexistence_detail(QString::from(&detail));
                qobject.as_mut().set_busy(false);
            });
        });
    }
}

/// The settings write lock and the forced-mode override
/// (`crate::settings_learning`): every write of the settings file goes through
/// it, so a failed restore of a learning window cannot leave Allow applied.
pub(crate) static SETTINGS_WRITE: SettingsStore = SettingsStore::new();

/// Running time of the GUI since the last learning tick.
static TICKER: std::sync::Mutex<Ticker> = std::sync::Mutex::new(Ticker::new());

/// True while the settings file could not be written and an in-process
/// override holds the silent mode (the `settingsWriteFailed` property's source;
/// also what the headless probe waits for).
pub fn settings_write_failed() -> bool {
    SETTINGS_WRITE.write_failed()
}

/// The silent mode to push to the silent-answer state: `requested`, unless the
/// forced-mode override holds (`BridgeFeed.setSilentState`).
pub(crate) fn forced_or(requested: SilentMode) -> SilentMode {
    SETTINGS_WRITE.honor_mode(requested)
}

/// How long Stop waits for the saved list to be written (it is off the Qt thread).
const LEARNED_FLUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the worker thread publishes at once after a settings transaction, not
/// waiting for the Qt thread:
/// * while the override holds, the silent-answer state (the property push that
///   follows comes a moment later), so an expired window's Allow is not
///   answered on in between;
/// * a window that is over (or not in force) stops the recording and saves
///   what was seen. Only ever to false, and only from the newest snapshot
///   (`SettingsStore::ends_learning`): starting one is applied in order on the
///   Qt thread (`apply_preferences`), and a late stale snapshot must not stop
///   what a newer one started.
fn push_forced_state(applied: &Applied) {
    if SETTINGS_WRITE.write_failed() {
        crate::silent_mode::set_state(SETTINGS_WRITE.honor_mode(SilentMode::Off));
    }
    // Under the settings gate: a Start applied on the Qt thread meanwhile
    // cannot be followed by this switching recording off.
    SETTINGS_WRITE.end_learning_if_latest(applied, || {
        crate::learned::set_active(false);
    });
}

impl qobject::SettingsController {
    /// Read-modify-write the settings file off the UI thread (other persisted
    /// fields are kept), then publish what is in force. `explicit` is the
    /// silent mode the owner just chose, if this is that change.
    fn persist_preferences(
        self: Pin<&mut Self>,
        explicit: Option<SilentMode>,
        change: impl FnOnce(&mut Settings) + Send + 'static,
    ) {
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            if crate::demo::enabled() {
                // Demo mode never reads or writes the real settings file.
                let mut settings = Settings::default();
                change(&mut settings);
                let _ = qt_thread.queue(move |qobject| qobject.apply_preferences(&settings));
                return;
            }
            let actual =
                SETTINGS_WRITE.transact(&settings_path(), SystemClocks.wall_ms(), explicit, change);
            push_forced_state(&actual);
            let _ = qt_thread.queue(move |qobject| qobject.apply_if_newest(&actual));
        });
    }

    /// Apply a transaction's snapshot unless a newer one was applied already
    /// (queue order is not transaction order) or this is demo mode.
    fn apply_if_newest(self: Pin<&mut Self>, applied: &Applied) {
        let _gate = SETTINGS_WRITE.gate();
        if SETTINGS_WRITE.claim(applied.generation, crate::demo::enabled()) {
            self.apply_preferences(&applied.settings);
        }
    }

    fn apply_preferences(mut self: Pin<&mut Self>, settings: &Settings) {
        // The override has the last word: Allow is never applied while it holds.
        let settings = &SETTINGS_WRITE.honor(settings.clone());
        // Before the silent mode property moves: its change sweeps the waiting
        // rows, and an Allow answered then must already be recorded.
        self.as_mut().publish_learning(settings);
        self.as_mut()
            .set_settings_write_failed(SETTINGS_WRITE.write_failed());
        self.as_mut()
            .set_default_duration(QString::from(settings.duration_token()));
        let mode = settings.silent_mode.map_or("", SilentMode::token);
        self.as_mut().set_silent_mode(QString::from(mode));
        self.as_mut().set_preferences_loaded(true);
    }

    /// Tell the recorder and the banner whether a window runs. A window that
    /// just began sweeps the rows already waiting.
    fn publish_learning(mut self: Pin<&mut Self>, settings: &Settings) {
        let window = settings.learning.as_ref();
        if crate::learned::set_active(window.is_some()) {
            crate::silent_mode::learning_started();
        }
        let now = SystemClocks.wall_ms();
        let ends = window.map_or(0, |w| now.saturating_add(learning::remaining_ms(w, now)));
        self.as_mut().set_learning_active(window.is_some());
        self.as_mut().set_learning_ends_ms(ends);
        self.refresh_learning_status();
    }

    /// Apply an autostart read/write outcome from a queued `CxxQtThread`
    /// closure. `error` is `None` on success (or a plain refresh).
    fn apply_autostart_result(mut self: Pin<&mut Self>, enabled: bool, error: Option<String>) {
        self.as_mut().set_autostart_enabled(enabled);
        self.as_mut()
            .set_autostart_error(QString::from(&error.unwrap_or_default()));
        self.as_mut().set_busy(false);
    }
}
