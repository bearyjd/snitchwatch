//! `BridgeFeed` — the QML-facing hub for the live bridge wiring (Task 13).
//!
//! Two responsibilities, both thin:
//!   * **Status surface.** `ok` / `statusText` reflect
//!     [`crate::bridge_runtime::status`] so `main.qml` can bind a
//!     `Kirigami.InlineMessage` when the external service is unavailable — the
//!     window still opens either way (no panic, no silent death).
//!   * **Inbound dispatcher.** Two QML entry points converge on one typed
//!     `dispatch`: `sendClientJson(json)` is the sink the models' request
//!     signals (`subscriptionRequested` / `ruleChangeRequested`) connect to
//!     and deserializes first, while `submitVerdict(...)` builds the message
//!     directly from stable tokens via [`crate::pending_decision`]. Both push
//!     onto the bridge's inbound pump — the exact channel a WebSocket client
//!     frame would feed, so verdict resolution and rule effects behave
//!     identically to the WS path.
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
        /// True while the external bridge service is connected; false while it
        /// is reconnecting (or has not been started in a headless QML test).
        #[qproperty(bool, ok)]
        /// Human-readable status line for the app-level `InlineMessage`.
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        type BridgeFeed = super::BridgeFeedRust;

        /// Refresh `ok` / `statusText` from the bridge runtime's current state.
        /// Called from `main.qml`'s `Component.onCompleted`; `main` has already
        /// started the client, so this is a pure read of its current state.
        #[qinvokable]
        fn refresh(self: Pin<&mut BridgeFeed>);

        /// Deserialize a model-emitted `ClientMessage` JSON and push it onto the
        /// bridge's inbound pump. Malformed JSON or a stopped bridge is logged
        /// and dropped — never panics, never sends a wrong message.
        #[qinvokable]
        #[cxx_name = "sendClientJson"]
        fn send_client_json(self: Pin<&mut BridgeFeed>, json: &QString);

        /// Build and dispatch a verdict from stable QML tokens. Keeping this
        /// in the feed removes the QML signal relay from the safety-critical
        /// button path while retaining `pending_decision` as the single wire
        /// shape/source of conservative token parsing.
        #[qinvokable]
        #[cxx_name = "submitVerdict"]
        fn submit_verdict(
            self: Pin<&mut BridgeFeed>,
            row_id: &QString,
            choice: &QString,
            scope: &QString,
            duration: &QString,
        );
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
        match crate::bridge_dispatch::decode_client(&json) {
            Ok(msg) => dispatch(msg),
            Err(e) => {
                tracing::warn!(error = %e, %json, "BridgeFeed: bad ClientMessage JSON, dropped")
            }
        }
    }

    fn submit_verdict(
        self: Pin<&mut Self>,
        row_id: &QString,
        choice: &QString,
        scope: &QString,
        duration: &QString,
    ) {
        match crate::pending_decision::build_verdict_message(
            &row_id.to_string(),
            &choice.to_string(),
            &scope.to_string(),
            &duration.to_string(),
        ) {
            Some(msg) => dispatch(msg),
            None => {
                tracing::warn!(choice = %choice.to_string(), "BridgeFeed: unrecognised verdict choice")
            }
        }
    }
}

/// Push a typed message onto the bridge's inbound pump — the exact channel a
/// WebSocket client frame feeds. Both QML entry points converge here already
/// typed, so a verdict never round-trips through JSON just to be re-parsed.
fn dispatch(msg: snitchwatch_bridge::ws_messages::ClientMessage) {
    let Some(handles) = crate::bridge_runtime::handles() else {
        tracing::warn!("BridgeFeed: bridge not running; dropping client message");
        return;
    };
    if let Err(error) = dispatch_to(&handles, msg) {
        tracing::warn!(error = %error, "BridgeFeed: client mutation dropped");
    }
}

pub(crate) fn dispatch_to(
    handles: &crate::bridge_runtime::BridgeHandles,
    mut msg: snitchwatch_bridge::ws_messages::ClientMessage,
) -> Result<(), crate::bridge_runtime::SendClientMessageError> {
    // A verdict or configuration change applies to the service instance that
    // supplied the UI state. Never queue it across a disconnect: a restarted
    // bridge may have different pending rows, rules, or profile state.
    // Do not await on the Qt thread. `try_send` also rejects a saturated
    // channel instead of retaining a mutation long enough to cross a service
    // restart.
    if let snitchwatch_bridge::ws_messages::ClientMessage::SetVerdict { row_id, .. } = &mut msg {
        let Some((session, wire_id)) = split_session_row_id(row_id) else {
            return Err(crate::bridge_runtime::SendClientMessageError::StaleSession);
        };
        *row_id = wire_id.to_owned();
        handles.try_send_for_session(session, msg)
    } else {
        handles.try_send(msg)
    }
}

/// Local-only row identity. Never transmitted to the service.
fn split_session_row_id(id: &str) -> Option<(u64, &str)> {
    let (session, wire_id) = id.split_once(':')?;
    let session = session.parse::<u64>().ok().filter(|id| *id != 0)?;
    (!wire_id.is_empty()).then_some((session, wire_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_row_identity_retains_origin_even_when_wire_ids_are_reused() {
        assert_eq!(split_session_row_id("1:7"), Some((1, "7")));
        assert_eq!(split_session_row_id("2:7"), Some((2, "7")));
        for id in ["7", "0:7", "invalid:7", "2:"] {
            assert_eq!(split_session_row_id(id), None);
        }
    }
}
