//! `WizardController` — first-run onboarding wizard surface (Task 12).
//!
//! Thin cxx-qt wrapper around [`crate::wizard`]'s `detect_daemon_state`/
//! `DaemonState` (a near-verbatim, zero-Tauri-dependency port of
//! `snitchwatch-tauri::wizard`, whose 5 unit tests transfer unchanged) and
//! `start_unit_via_systemctl` (ported from
//! `snitchwatch-tauri::commands::start_daemon_unit`). This file only adds
//! the QML-facing surface: an async `probe()` re-detect and a `startUnit()`
//! action, both off the UI thread.
//!
//! `probe()`/`startUnit()` prefer the in-process bridge's own Tokio runtime
//! (`bridge_runtime::handles()`), spawning onto it exactly like
//! `TrafficModel::start_bridge_feed` does, and queuing the result back via
//! `CxxQtThread` (Task 1's async-signal-emission pattern). If the bridge
//! itself failed to start — the exact case this wizard exists to surface —
//! there is no shared runtime to borrow, so both fall back to a scratch
//! thread + a throwaway current-thread runtime for that one call. Either way
//! the Qt/UI thread is never blocked, and neither a missing `systemctl`
//! binary nor a dial failure ever panics: both resolve to an ordinary
//! `DaemonState`/`Err` string surfaced in `detail`.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

use crate::wizard::{detect_daemon_state, start_unit_via_systemctl, DaemonState};

/// Overridable via the same env var `bridge_runtime::bridge_config` honors,
/// so a non-default gRPC bind still gets probed correctly.
const GRPC_ENDPOINT_ENV: &str = "SNITCHWATCH_GRPC_BIND";
const DEFAULT_GRPC_ENDPOINT: &str = "127.0.0.1:50051";

fn grpc_endpoint() -> String {
    std::env::var(GRPC_ENDPOINT_ENV).unwrap_or_else(|_| DEFAULT_GRPC_ENDPOINT.to_string())
}

fn state_name(state: DaemonState) -> &'static str {
    match state {
        DaemonState::Connected => "connected",
        DaemonState::UnitMissing => "unitMissing",
        DaemonState::UnitInactive => "unitInactive",
        DaemonState::UnreachableRetrying => "unreachableRetrying",
    }
}

fn state_detail(state: DaemonState) -> &'static str {
    match state {
        DaemonState::Connected => "Connected to the Snitchwatch bridge.",
        DaemonState::UnitMissing => {
            "The snitchwatch-opensnitchd.service unit isn't installed. See the \
             rpm-ostree layering guide (docs/packaging/rpm-ostree-layering.md) or \
             the bluebuild batteries-included image to install the daemon."
        }
        DaemonState::UnitInactive => {
            "The daemon service is installed but not running. Start it to continue."
        }
        DaemonState::UnreachableRetrying => {
            "The daemon service looks enabled, but the bridge can't reach it yet. \
             Retrying automatically in the background."
        }
    }
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// First-run onboarding wizard surface, bound by `OnboardingPage.qml`.
        #[qobject]
        #[qml_element]
        /// One of `connected` / `unitMissing` / `unitInactive` /
        /// `unreachableRetrying`, mirroring [`super::DaemonState`] (Task 12).
        #[qproperty(QString, state)]
        /// True while a `probe()`/`startUnit()` call is in flight.
        #[qproperty(bool, busy)]
        /// Human-readable detail/error text for the current state.
        #[qproperty(QString, detail)]
        type WizardController = super::WizardControllerRust;

        /// Re-run `detect_daemon_state` off the UI thread; updates `state`/
        /// `detail` and clears `busy` once resolved. Called from
        /// `OnboardingPage.qml`'s `Component.onCompleted` and its retry
        /// button/backoff timer.
        #[qinvokable]
        fn probe(self: Pin<&mut WizardController>);

        /// `systemctl --user start snitchwatch-opensnitchd.service`, wired to
        /// the `UnitInactive` state's "Start daemon" CTA. Re-probes on
        /// completion (success or failure) so `state`/`detail` reflect the
        /// outcome.
        #[qinvokable]
        #[cxx_name = "startUnit"]
        fn start_unit(self: Pin<&mut WizardController>);
    }

    impl cxx_qt::Threading for WizardController {}
}

/// Rust-side state for [`qobject::WizardController`].
pub struct WizardControllerRust {
    state: QString,
    busy: bool,
    detail: QString,
}

impl Default for WizardControllerRust {
    fn default() -> Self {
        // Assume connected until the first `probe()` resolves otherwise, so a
        // genuinely healthy daemon never flashes an onboarding page it
        // doesn't need (`main.qml` only pushes the page on a `stateChanged`
        // signal, which won't fire if this optimistic default turns out to
        // be correct).
        Self {
            state: QString::from("connected"),
            busy: false,
            detail: QString::from("Detecting the Snitchwatch daemon\u{2026}"),
        }
    }
}

impl qobject::WizardController {
    fn probe(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        let endpoint = grpc_endpoint();

        if let Some(handles) = crate::bridge_runtime::handles() {
            handles.runtime().spawn(async move {
                let state = detect_daemon_state(&endpoint).await;
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_probe_result(state);
                });
            });
        } else {
            spawn_scratch_probe(qt_thread, endpoint);
        }
    }

    fn start_unit(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();

        std::thread::spawn(move || {
            let outcome = start_unit_via_systemctl();
            let _ = qt_thread.queue(move |qobject| match outcome {
                Ok(()) => qobject.probe(),
                Err(message) => qobject.apply_probe_error(&message),
            });
        });
    }
}

impl qobject::WizardController {
    /// Update `state`/`detail` from a resolved [`DaemonState`] and clear
    /// `busy`. Called from a queued `CxxQtThread` closure, never directly
    /// from QML.
    fn apply_probe_result(mut self: Pin<&mut Self>, state: DaemonState) {
        self.as_mut().set_state(QString::from(state_name(state)));
        self.as_mut().set_detail(QString::from(state_detail(state)));
        self.as_mut().set_busy(false);
    }

    /// Surface an action failure (e.g. `systemctl` missing/non-zero exit, or
    /// the scratch-runtime build failing) without changing `state` — the
    /// wizard stays on whatever state it was showing, just with an updated
    /// detail line.
    fn apply_probe_error(mut self: Pin<&mut Self>, message: &str) {
        self.as_mut().set_detail(QString::from(message));
        self.as_mut().set_busy(false);
    }
}

/// Run `detect_daemon_state` on a scratch thread + throwaway current-thread
/// Tokio runtime, for when the bridge's own runtime never came up (the exact
/// case this wizard exists to surface — see module docs).
fn spawn_scratch_probe(
    qt_thread: cxx_qt::CxxQtThread<qobject::WizardController>,
    endpoint: String,
) {
    std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                let state = rt.block_on(detect_daemon_state(&endpoint));
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_probe_result(state);
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "WizardController: scratch runtime build failed");
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_probe_error(&format!("internal error: {e}"));
                });
            }
        }
    });
}
