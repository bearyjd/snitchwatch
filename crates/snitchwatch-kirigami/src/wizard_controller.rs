//! Onboarding observes the existing authenticated GUI bridge connection.
//! Probing is a quick local state read; daemon liveness comes separately
//! from the diagnostics feed. The GUI never starts or stops services.

use core::pin::Pin;
use cxx_qt_lib::QString;

use crate::wizard::{state_from_authenticated_bridge, DaemonState};

fn state_name(state: DaemonState) -> &'static str {
    match state {
        DaemonState::Connected => "connected",
        DaemonState::UnreachableRetrying => "unreachableRetrying",
    }
}

fn state_detail(state: DaemonState) -> &'static str {
    match state {
        DaemonState::Connected => "Connected to the Snitchwatch bridge.",
        DaemonState::UnreachableRetrying => {
            "Can't reach the Snitchwatch system bridge. It runs as a system \
             service; this app can't start it. Check that your account is in the \
             snitchwatch-ui group (log out and back in after adding it) and that \
             the bridge sockets are running. The client retries automatically."
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
        /// One of `connected` / `unreachableRetrying`, mirroring [`super::DaemonState`] (Task 12).
        #[qproperty(QString, state)]
        /// True while a `probe()` call is in flight.
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
    }
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
        // Demo mode has no bridge by design; keep onboarding out of the way.
        if crate::demo::enabled() {
            return;
        }
        self.as_mut().set_busy(true);
        let connected =
            crate::bridge_runtime::handles().is_some_and(|handles| handles.is_connected());
        self.apply_probe_result(state_from_authenticated_bridge(connected));
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
}
