//! `BridgeFeed` — the QML-facing hub for the live bridge wiring (Task 13).
//!
//! Two responsibilities, both thin:
//!   * **Status surface.** `ok` / `statusText` reflect
//!     [`crate::bridge_runtime::status`] so `main.qml` can bind a
//!     `Kirigami.InlineMessage` that appears only when the bridge failed to
//!     start — the window still opens either way (no panic, no silent death).
//!   * **Inbound dispatcher.** `sendClientJson(json)` is the single sink the
//!     models' request signals (`verdictSubmitted` / `subscriptionRequested` /
//!     `ruleChangeRequested`) connect to. It deserializes to a typed
//!     `ClientMessage` and pushes it onto the bridge's inbound pump — the exact
//!     channel a WebSocket client frame would feed, so verdict resolution and
//!     rule effects behave identically to the WS path.
//!
//! The outbound direction (bridge → models) is *not* here: each model owns its
//! own `startBridgeFeed()` feed task, because each must run its `RowStore`
//! mutations behind its own `QAbstractListModel` begin/end signals on the Qt
//! thread. This object never touches the models directly.

use core::pin::Pin;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Live-wiring hub bound by `main.qml`.
        #[qobject]
        #[qml_element]
        /// True once the in-process bridge is running; false if it failed to
        /// start (or has not been started, e.g. in a headless QML test).
        #[qproperty(bool, ok)]
        /// Human-readable status line for the app-level `InlineMessage`.
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        type BridgeFeed = super::BridgeFeedRust;

        /// Refresh `ok` / `statusText` from the bridge runtime's current state.
        /// Called from `main.qml`'s `Component.onCompleted`; `main` has already
        /// run startup, so this is a pure read of the outcome.
        #[qinvokable]
        fn refresh(self: Pin<&mut BridgeFeed>);

        /// Deserialize a model-emitted `ClientMessage` JSON and push it onto the
        /// bridge's inbound pump. Malformed JSON or a stopped bridge is logged
        /// and dropped — never panics, never sends a wrong message.
        #[qinvokable]
        #[cxx_name = "sendClientJson"]
        fn send_client_json(self: Pin<&mut BridgeFeed>, json: &QString);
    }
}

/// Rust-side state for [`qobject::BridgeFeed`].
#[derive(Default)]
pub struct BridgeFeedRust {
    ok: bool,
    status_text: QString,
}

impl qobject::BridgeFeed {
    fn refresh(mut self: Pin<&mut Self>) {
        let (ok, msg) = match crate::bridge_runtime::status() {
            Some(status) => status,
            None => (false, "Bridge not started".to_string()),
        };
        self.as_mut().set_ok(ok);
        self.as_mut().set_status_text(QString::from(&msg));
    }

    fn send_client_json(self: Pin<&mut Self>, json: &QString) {
        let json = json.to_string();
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("BridgeFeed: bridge not running; dropping client message");
            return;
        };
        let msg = match crate::bridge_dispatch::decode_client(&json) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(error = %e, %json, "BridgeFeed: bad ClientMessage JSON, dropped");
                return;
            }
        };
        // Push onto the bridge's runtime — `mpsc::Sender::send` is async, and we
        // must never block the Qt thread waiting on the channel.
        let tx = handles.inbound_tx();
        handles.runtime().spawn(async move {
            if tx.send(msg).await.is_err() {
                tracing::warn!("BridgeFeed: inbound channel closed; client message dropped");
            }
        });
    }
}
