//! `TrayController` — system tray state feed (Task 18).
//!
//! **Verified before building:** `qmake6 -query QT_INSTALL_QML` resolves to
//! `/usr/lib64/qt6/qml` in this environment, and
//! `Qt/labs/platform/liblabsplatformplugin.so` exists there — the
//! `Qt.labs.platform.SystemTrayIcon` primary path from the plan is available,
//! so no `KStatusNotifierItem`/`cxx-kde-frameworks` fallback work was needed.
//! The tray icon/menu itself is declared directly in `main.qml`; this
//! `QObject` only feeds it the tooltip/menu-label strings derived from the
//! bridge's `TrayState` by [`crate::tray`]'s pure, unit-tested functions
//! (ported unchanged from `snitchwatch-tauri::tray`).

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

use crate::tray::{derive_menu_label, derive_tooltip, menu_label_token};

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
        /// Tooltip text for the current `TrayState` (`derive_tooltip`).
        #[qproperty(QString, tooltip)]
        /// One of `default` / `pause_filtering` / `resume_filtering` /
        /// `reconnect` (`derive_menu_label`'s token, see
        /// `crate::tray::menu_label_token`), so the tray's context-menu item
        /// text/action can react to daemon/filter state without duplicating
        /// the derivation logic in QML.
        #[qproperty(QString, menu_label, cxx_name = "menuLabel")]
        type TrayController = super::TrayControllerRust;

        /// Start the live feed: subscribe to the bridge's tray-state watch
        /// channel and update `tooltip`/`menuLabel` on every change. No-op
        /// when the bridge isn't running. Called from `main.qml`'s
        /// `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut TrayController>);
    }

    impl cxx_qt::Threading for TrayController {}
}

/// Rust-side state for [`qobject::TrayController`].
pub struct TrayControllerRust {
    tooltip: QString,
    menu_label: QString,
}

impl Default for TrayControllerRust {
    fn default() -> Self {
        Self {
            tooltip: QString::from("Snitchwatch — filtering"),
            menu_label: QString::from("default"),
        }
    }
}

impl qobject::TrayController {
    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(mut rx) = crate::bridge_runtime::tray_rx() else {
            tracing::warn!("TrayController: bridge not running; live feed disabled");
            return;
        };
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("TrayController: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();

        // Apply the current (initial) state immediately, then react to every
        // subsequent change — mirrors the Tauri shell's `Tray::install`,
        // which rendered `TrayState::Idle` up front before its watch loop.
        let initial = rx.borrow().clone();
        let _ = qt_thread.queue(move |qobject| {
            qobject.apply_tray_state(initial);
        });

        handles.runtime().spawn(async move {
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let state = rx.borrow().clone();
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_tray_state(state);
                });
            }
        });
    }
}

impl qobject::TrayController {
    fn apply_tray_state(mut self: Pin<&mut Self>, state: crate::bridge_runtime::BridgeTrayState) {
        self.as_mut()
            .set_tooltip(QString::from(&derive_tooltip(&state)));
        let label = derive_menu_label(&state);
        self.as_mut()
            .set_menu_label(QString::from(menu_label_token(&label)));
    }
}
