//! `SettingsController` — autostart toggle + crash-log surfacing (Tasks 15,
//! 16), bound by `DiagnosticsPage.qml`.
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
use crate::crash_log::read_crash_log_tail;
use crate::paths::{autostart_path, crash_log_path, settings_path};
use crate::settings::{read_settings, write_settings};

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
    }

    impl cxx_qt::Threading for SettingsController {}
}

/// Rust-side state for [`qobject::SettingsController`].
pub struct SettingsControllerRust {
    autostart_enabled: bool,
    autostart_error: QString,
    crash_log_text: QString,
    rdap_enabled: bool,
    busy: bool,
}

impl Default for SettingsControllerRust {
    fn default() -> Self {
        Self {
            autostart_enabled: false,
            autostart_error: QString::default(),
            crash_log_text: QString::from("Loading\u{2026}"),
            rdap_enabled: false,
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
            let path = settings_path();
            // Preserve any other persisted fields rather than clobbering
            // them with a lone-field write.
            let mut settings = read_settings(&path);
            settings.rdap_enabled = enabled;
            let write_err = write_settings(&path, &settings).err();
            if let Some(e) = write_err {
                tracing::warn!(error = %e, "failed to persist RDAP setting");
            }
            // Re-read from disk rather than trusting `enabled` blindly, so a
            // failed write still reports the setting's actual on-disk state.
            let actual = read_settings(&path).rdap_enabled;
            let _ = qt_thread.queue(move |mut qobject| {
                qobject.as_mut().set_rdap_enabled(actual);
                qobject.as_mut().set_busy(false);
            });
        });
    }
}

impl qobject::SettingsController {
    /// Apply an autostart read/write outcome from a queued `CxxQtThread`
    /// closure. `error` is `None` on success (or a plain refresh).
    fn apply_autostart_result(mut self: Pin<&mut Self>, enabled: bool, error: Option<String>) {
        self.as_mut().set_autostart_enabled(enabled);
        self.as_mut()
            .set_autostart_error(QString::from(&error.unwrap_or_default()));
        self.as_mut().set_busy(false);
    }
}
