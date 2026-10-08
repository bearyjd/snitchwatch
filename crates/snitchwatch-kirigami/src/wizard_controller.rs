//! Onboarding observes the existing authenticated GUI bridge connection.
//! Probing is a quick local state read; daemon liveness comes separately
//! from the diagnostics feed. Explicit service-start actions run off the
//! UI thread and use normal systemd authorization.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

use crate::wizard::{start_unit_via_systemctl, state_from_authenticated_bridge, DaemonState};

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
            "The system OpenSnitch service isn't installed. See the Snitchwatch \
             system bridge integration guide to install the host services."
        }
        DaemonState::UnitInactive => {
            "The daemon service is installed but not running. Start it to continue."
        }
        DaemonState::UnreachableRetrying => {
            "Waiting for the Snitchwatch bridge. Check the service setup and \
             GUI access permissions. The client retries automatically."
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

        /// Observe the authenticated GUI bridge connection; updates `state`/
        /// `detail` and clears `busy` once resolved. Called from
        /// `OnboardingPage.qml`'s `Component.onCompleted` and its retry
        /// button/backoff timer.
        #[qinvokable]
        fn probe(self: Pin<&mut WizardController>);

        /// `systemctl --system start opensnitch.service`, wired to
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
        let connected =
            crate::bridge_runtime::handles().is_some_and(|handles| handles.is_connected());
        self.apply_probe_result(state_from_authenticated_bridge(connected));
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
    /// authorization being denied) without changing `state` — the
    /// wizard stays on whatever state it was showing, just with an updated
    /// detail line.
    fn apply_probe_error(mut self: Pin<&mut Self>, message: &str) {
        self.as_mut().set_detail(QString::from(message));
        self.as_mut().set_busy(false);
    }
}
