//! `TrayController` — system tray state feed (Task 18).
//!
//! **Verified before building:** `qmake6 -query QT_INSTALL_QML` resolves to
//! `/usr/lib64/qt6/qml` in this environment, and
//! `Qt/labs/platform/liblabsplatformplugin.so` exists there — the
//! `Qt.labs.platform.SystemTrayIcon` primary path from the plan is available,
//! so no `KStatusNotifierItem`/`cxx-kde-frameworks` fallback work was needed.
//! The tray icon/menu itself is declared directly in `main.qml`; this
//! `QObject` only feeds it the tooltip/menu-label strings derived from the
//! bridge's `TrayState` and `FilterPauseState` by [`crate::tray`]'s pure,
//! unit-tested functions.

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QDateTime, QString, QTimeZone};

use crate::bridge_runtime::{BridgePauseState, BridgeTrayState};
use crate::tray::{
    build_set_filtering_paused_json, derive_menu_label, derive_tooltip, menu_label_token,
};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Tray icon state feed, bound by the `Qt.labs.platform.SystemTrayIcon`
        /// declared in `main.qml`.
        #[qobject]
        #[qml_element]
        /// Tooltip text for the current tray and pause state (`derive_tooltip`).
        #[qproperty(QString, tooltip)]
        /// One of `default` / `pause_filtering` / `resume_filtering` /
        /// `reconnect` (`derive_menu_label`'s token, see
        /// `crate::tray::menu_label_token`), so the tray's context-menu items
        /// can react to daemon/filter state without duplicating the
        /// derivation logic in QML.
        #[qproperty(QString, menu_label, cxx_name = "menuLabel")]
        /// Local end time ("HH:MM") of the active filtering pause, or empty
        /// when not paused or unknown (issue #47).
        #[qproperty(QString, paused_until, cxx_name = "pausedUntil")]
        type TrayController = super::TrayControllerRust;

        /// Start the live feed: subscribe to the bridge's tray-state and
        /// pause-state watch channels and update the properties on every
        /// change. No-op when the bridge isn't running. Called from
        /// `main.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut TrayController>);

        /// Emitted with the JSON-encoded `ClientMessage::SetFilteringPaused`
        /// for the tray menu's pause/resume items. `main.qml` connects this
        /// to `bridgeFeed.sendClientJson` — the same in-process path
        /// `BridgeFeed::submitVerdict` uses.
        #[qsignal]
        #[cxx_name = "filteringToggleRequested"]
        fn filtering_toggle_requested(self: Pin<&mut TrayController>, json: QString);

        /// Pause filtering for `secs` seconds (bridge auto-allows every
        /// `AskRule` until then). The tray offers 5 min, 30 min and 1 hour;
        /// the bridge rejects any other length (issue #47).
        #[qinvokable]
        #[cxx_name = "pauseFor"]
        fn pause_for(self: Pin<&mut TrayController>, secs: i32);

        /// End the pause now and resume normal prompting.
        #[qinvokable]
        fn resume(self: Pin<&mut TrayController>);
    }

    impl cxx_qt::Threading for TrayController {}
}

/// Rust-side state for [`qobject::TrayController`].
pub struct TrayControllerRust {
    tooltip: QString,
    menu_label: QString,
    paused_until: QString,
    tray_state: BridgeTrayState,
    pause_state: BridgePauseState,
}

impl Default for TrayControllerRust {
    fn default() -> Self {
        Self {
            tooltip: QString::from("Snitchwatch — filtering"),
            menu_label: QString::from("default"),
            paused_until: QString::default(),
            tray_state: BridgeTrayState::Idle,
            pause_state: BridgePauseState::NOT_PAUSED,
        }
    }
}

impl qobject::TrayController {
    fn start_bridge_feed(self: Pin<&mut Self>) {
        let (Some(mut tray_rx), Some(mut pause_rx), Some(handles)) = (
            crate::bridge_runtime::tray_rx(),
            crate::bridge_runtime::pause_rx(),
            crate::bridge_runtime::handles(),
        ) else {
            tracing::warn!("TrayController: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();

        // Apply the current (initial) state immediately, then react to every
        // subsequent change — mirrors the Tauri shell's `Tray::install`,
        // which rendered `TrayState::Idle` up front before its watch loop.
        let initial_tray = tray_rx.borrow().clone();
        let initial_pause = pause_rx.borrow().clone();
        let initial_handles = handles.clone();
        let _ = qt_thread.queue(move |mut qobject| {
            if initial_handles.is_current_session(initial_tray.connection_id) {
                qobject.as_mut().rust_mut().tray_state = initial_tray.state;
            }
            if initial_handles.is_current_session(initial_pause.connection_id) {
                qobject.as_mut().rust_mut().pause_state = initial_pause.state;
            }
            qobject.refresh();
        });

        // Each change is applied only if it still belongs to the live bridge
        // session when the queued Qt callback runs.
        let runtime = handles.runtime().clone();
        let tray_thread = qt_thread.clone();
        let tray_handles = handles.clone();
        runtime.spawn(async move {
            while tray_rx.changed().await.is_ok() {
                let received = tray_rx.borrow().clone();
                let session_handles = tray_handles.clone();
                let _ = tray_thread.queue(move |mut qobject| {
                    if session_handles.is_current_session(received.connection_id) {
                        qobject.as_mut().rust_mut().tray_state = received.state;
                        qobject.refresh();
                    }
                });
            }
        });
        runtime.spawn(async move {
            while pause_rx.changed().await.is_ok() {
                let received = pause_rx.borrow().clone();
                let session_handles = handles.clone();
                let _ = qt_thread.queue(move |mut qobject| {
                    if session_handles.is_current_session(received.connection_id) {
                        qobject.as_mut().rust_mut().pause_state = received.state;
                        qobject.refresh();
                    }
                });
            }
        });
    }
}

impl qobject::TrayController {
    fn pause_for(self: Pin<&mut Self>, secs: i32) {
        let Ok(secs) = u64::try_from(secs) else {
            tracing::warn!(secs, "TrayController: ignoring a negative pause length");
            return;
        };
        let json = build_set_filtering_paused_json(true, Some(secs));
        self.filtering_toggle_requested(QString::from(&json));
    }

    fn resume(self: Pin<&mut Self>) {
        let json = build_set_filtering_paused_json(false, None);
        self.filtering_toggle_requested(QString::from(&json));
    }
}

impl qobject::TrayController {
    fn refresh(mut self: Pin<&mut Self>) {
        let pause = self.rust().pause_state;
        let until = match (pause.paused, pause.expires_at_unix_ms) {
            (true, Some(ms)) => local_hh_mm(ms),
            _ => String::new(),
        };
        let tooltip = derive_tooltip(&self.rust().tray_state, &pause, &until);
        let label = derive_menu_label(&self.rust().tray_state, &pause);
        self.as_mut().set_tooltip(QString::from(&tooltip));
        self.as_mut()
            .set_menu_label(QString::from(menu_label_token(&label)));
        self.as_mut().set_paused_until(QString::from(&until));
    }
}

/// `unix_ms` as a local "HH:MM" time, in the system time zone.
fn local_hh_mm(unix_ms: u64) -> String {
    let ms = i64::try_from(unix_ms).unwrap_or(i64::MAX);
    let time_zone = QTimeZone::system_time_zone();
    QDateTime::from_msecs_since_epoch(ms, &time_zone)
        .time()
        .format(&QString::from("HH:mm"))
        .to_string()
}
