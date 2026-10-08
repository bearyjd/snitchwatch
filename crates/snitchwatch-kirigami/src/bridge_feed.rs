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
//! thread. This object never touches the models directly. The one outbound
//! event it relays is model-free: `verdictNotRemembered` (issue #44), which
//! `ConnectionsPage.qml` turns into a passive notification.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use snitchwatch_bridge::ws_messages::ServerMessage;

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
        /// shape/source of conservative token parsing. Returns whether it was
        /// queued for the row's live bridge session (false after a
        /// disconnect, for a stale row, or without a bridge), so the page can
        /// say a Deny wasn't sent rather than guess why it wasn't remembered.
        #[qinvokable]
        #[cxx_name = "submitVerdict"]
        fn submit_verdict(
            self: Pin<&mut BridgeFeed>,
            row_id: &QString,
            choice: &QString,
            scope: &QString,
            duration: &QString,
        ) -> bool;

        /// Whether the bridge session row `row_id` came from advertised
        /// app-bound rules and is still the live session — read at click
        /// time, not polled, so a reconnect to an older bridge can't be
        /// mistaken for a capable one. False headless or for a malformed id.
        /// Gates the remembered inline Deny (`crate::inline_deny`).
        #[qinvokable]
        #[cxx_name = "appBoundRulesFor"]
        fn app_bound_rules_for(self: &BridgeFeed, row_id: &QString) -> bool;

        /// Issue #44: the bridge answered a remembered verdict for this
        /// connection only, because it couldn't identify the program's file.
        /// `row_id` is session-qualified like `ConnectionsModel`'s ids.
        #[qsignal]
        #[cxx_name = "verdictNotRemembered"]
        fn verdict_not_remembered(self: Pin<&mut BridgeFeed>, row_id: QString);

        /// Start relaying `verdictNotRemembered`. No-op when the bridge isn't
        /// running. Called from `main.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut BridgeFeed>);
    }

    impl cxx_qt::Threading for BridgeFeed {}
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

    fn app_bound_rules_for(&self, row_id: &QString) -> bool {
        app_bound_rules_for_row(
            crate::bridge_runtime::handles().as_ref(),
            &row_id.to_string(),
        )
    }

    fn send_client_json(self: Pin<&mut Self>, json: &QString) {
        let json = json.to_string();
        match crate::bridge_dispatch::decode_client(&json) {
            Ok(msg) => {
                dispatch(msg);
            }
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
    ) -> bool {
        match crate::pending_decision::build_verdict_message(
            &row_id.to_string(),
            &choice.to_string(),
            &scope.to_string(),
            &duration.to_string(),
        ) {
            Some(msg) => dispatch(msg),
            None => {
                tracing::warn!(choice = %choice.to_string(), "BridgeFeed: unrecognised verdict choice");
                false
            }
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("BridgeFeed: bridge not running; verdict notices disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        // An event, not state: no snapshot resync, so a lag only skips some.
        let mut rx = handles.subscribe();
        let runtime = handles.runtime().clone();
        runtime.spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let received = match rx.recv().await {
                    Ok(received) => received,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let connection_id = received.connection_id;
                let Some(row_id) = verdict_not_remembered_row(connection_id, &received.message)
                else {
                    continue;
                };
                let session_handles = handles.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if session_handles.is_current_session(connection_id) {
                        qobject.verdict_not_remembered(QString::from(&row_id));
                    }
                });
            }
        });
    }
}

/// The session-qualified row id (`<connection>:<wire id>`, as
/// `ConnectionsModel` stores it) of a `VerdictNotRemembered`, or `None` for
/// any other message.
fn verdict_not_remembered_row(connection_id: u64, msg: &ServerMessage) -> Option<String> {
    match msg {
        ServerMessage::VerdictNotRemembered { row_id, .. } => {
            Some(format!("{connection_id}:{row_id}"))
        }
        _ => None,
    }
}

/// Push a typed message onto the bridge's inbound pump — the exact channel a
/// WebSocket client frame feeds. Both QML entry points converge here already
/// typed, so a verdict never round-trips through JSON just to be re-parsed.
/// Returns whether the message was queued.
fn dispatch(msg: snitchwatch_bridge::ws_messages::ClientMessage) -> bool {
    let Some(handles) = crate::bridge_runtime::handles() else {
        tracing::warn!("BridgeFeed: bridge not running; dropping client message");
        return false;
    };
    match dispatch_to(&handles, msg) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(error = %error, "BridgeFeed: client mutation dropped");
            false
        }
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

/// Whether row `row_id`'s bridge session is live and advertised app-bound
/// rules (`appBoundRulesFor`). False without a runtime or for an id that
/// names no session: the inline Deny then stays once-only.
pub(crate) fn app_bound_rules_for_row(
    handles: Option<&crate::bridge_runtime::BridgeHandles>,
    row_id: &str,
) -> bool {
    match (handles, split_session_row_id(row_id)) {
        (Some(handles), Some((session, _))) => handles.advertises_app_bound_rules(session),
        _ => false,
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

    #[test]
    fn only_verdict_not_remembered_is_relayed_with_a_qualified_row_id() {
        let msg = ServerMessage::VerdictNotRemembered {
            row_id: "7".into(),
            reason: "fixed".into(),
        };
        assert_eq!(verdict_not_remembered_row(2, &msg), Some("2:7".to_string()));
        let other = ServerMessage::DenyScopeNarrowed {
            row_id: "7".into(),
            reason: "fixed".into(),
        };
        assert_eq!(verdict_not_remembered_row(2, &other), None);
        assert_eq!(
            verdict_not_remembered_row(2, &ServerMessage::ClearConnectionRows),
            None
        );
    }
}
