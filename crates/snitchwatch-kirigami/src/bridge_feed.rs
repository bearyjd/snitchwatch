//! `BridgeFeed` — the QML-facing hub for the live bridge wiring (Task 13).
//!
//! Two responsibilities, both thin:
//!   * **Status surface.** `ok` / `statusText` / `linkState` reflect
//!     [`crate::bridge_runtime::link_status`] so `main.qml` can bind a
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

use crate::bridge_runtime::{LinkState, LinkStatus};
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
        /// Which fixed sentence the app-level banner shows: `connecting`,
        /// `connected`, `retrying`, `failed` or `stopped`
        /// (`bridge_runtime::LinkState::token`). The banner reads this, never
        /// `statusText`, which carries error text.
        #[qproperty(QString, link_state, cxx_name = "linkState")]
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
        ///
        /// `bindable_process_path` is `ConnectionsModel.rowDetailsJson(row)`'s
        /// field of that name: whether the row's program has a file the
        /// bridge can bind a rule to. The feed can't look it up itself (the
        /// row store belongs to the model). Whatever `duration` asks for, a
        /// remembered verdict goes out once-only when it is false
        /// (`dispatch_to`).
        #[qinvokable]
        #[cxx_name = "submitVerdict"]
        fn submit_verdict(
            self: Pin<&mut BridgeFeed>,
            row_id: &QString,
            choice: &QString,
            scope: &QString,
            duration: &QString,
            bindable_process_path: bool,
        ) -> bool;

        /// Whether the bridge session row `row_id` came from advertised
        /// app-bound rules and is still the live session — read at click
        /// time, not polled, so a reconnect to an older bridge can't be
        /// mistaken for a capable one. False headless or for a malformed id.
        /// Gates the remembered inline Deny (`crate::inline_deny`).
        #[qinvokable]
        #[cxx_name = "appBoundRulesFor"]
        fn app_bound_rules_for(self: &BridgeFeed, row_id: &QString) -> bool;

        /// Whether row `row_id`'s live bridge session takes "Decide later"
        /// (`bridge_capabilities::DECIDE_LATER`, prompt-slot plan Part C),
        /// read at the time of use like `appBoundRulesFor`.
        #[qinvokable]
        #[cxx_name = "decideLaterFor"]
        fn decide_later_for(self: &BridgeFeed, row_id: &QString) -> bool;

        /// "Decide later" for row `row_id`: its bridge blocks the program for
        /// 5 minutes, or gives the daemon no answer when it can't name the
        /// program. Returns whether it was queued for the row's live session.
        #[qinvokable]
        #[cxx_name = "decideLater"]
        fn decide_later(self: Pin<&mut BridgeFeed>, row_id: &QString) -> bool;

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
pub struct BridgeFeedRust {
    ok: bool,
    status_text: QString,
    link_state: QString,
}

impl Default for BridgeFeedRust {
    fn default() -> Self {
        Self {
            ok: false,
            status_text: QString::default(),
            link_state: QString::from(LinkState::Connecting.token()),
        }
    }
}

/// What `BridgeFeed` publishes for the runtime's link status: `ok`,
/// `statusText` and `linkState`. Before the runtime exists the shell is just
/// starting.
fn published(link: Option<LinkStatus>) -> (bool, String, &'static str) {
    let link = link.unwrap_or(LinkStatus {
        state: LinkState::Connecting,
        detail: "Bridge not started".to_string(),
    });
    (
        link.state == LinkState::Connected,
        link.detail,
        link.state.token(),
    )
}

impl qobject::BridgeFeed {
    fn refresh(mut self: Pin<&mut Self>) {
        let (ok, detail, state) = published(crate::bridge_runtime::link_status());
        self.as_mut().set_ok(ok);
        self.as_mut().set_status_text(QString::from(&detail));
        self.as_mut().set_link_state(QString::from(state));
    }

    fn app_bound_rules_for(&self, row_id: &QString) -> bool {
        app_bound_rules_for_row(
            crate::bridge_runtime::handles().as_ref(),
            &row_id.to_string(),
        )
    }

    fn decide_later_for(&self, row_id: &QString) -> bool {
        decide_later_for_row(
            crate::bridge_runtime::handles().as_ref(),
            &row_id.to_string(),
        )
    }

    fn decide_later(self: Pin<&mut Self>, row_id: &QString) -> bool {
        let msg = snitchwatch_bridge::ws_messages::ClientMessage::DecideLater {
            row_id: row_id.to_string(),
        };
        dispatch(msg, false)
    }

    fn send_client_json(self: Pin<&mut Self>, json: &QString) {
        let json = json.to_string();
        match crate::bridge_dispatch::decode_client(&json) {
            // A row id in a JSON verdict names no program the caller vouches
            // for, so a verdict that asks to be remembered goes out once-only.
            Ok(msg) => {
                dispatch(msg, false);
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
        bindable_process_path: bool,
    ) -> bool {
        match crate::pending_decision::build_verdict_message(
            &row_id.to_string(),
            &choice.to_string(),
            &scope.to_string(),
            &duration.to_string(),
        ) {
            Some(msg) => dispatch(msg, bindable_process_path),
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
fn dispatch(
    msg: snitchwatch_bridge::ws_messages::ClientMessage,
    bindable_process_path: bool,
) -> bool {
    let Some(handles) = crate::bridge_runtime::handles() else {
        tracing::warn!("BridgeFeed: bridge not running; dropping client message");
        return false;
    };
    match dispatch_to(&handles, msg, bindable_process_path) {
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
    bindable_process_path: bool,
) -> Result<(), crate::bridge_runtime::SendClientMessageError> {
    // A verdict or configuration change applies to the service instance that
    // supplied the UI state. Never queue it across a disconnect: a restarted
    // bridge may have different pending rows, rules, or profile state.
    // Do not await on the Qt thread. `try_send` also rejects a saturated
    // channel instead of retaining a mutation long enough to cross a service
    // restart.
    //
    // Issues #44 and #72: whatever QML asked for, a verdict is not remembered
    // for a program the caller says has no bindable file, nor, for a host
    // scope, on a session without app-bound rules. Asked before `row_id` loses
    // its session prefix below. The sheet already offers no such choice, so a
    // downgrade here means one of its checks was bypassed (or the verdict came
    // as JSON, which vouches for no program).
    if let snitchwatch_bridge::ws_messages::ClientMessage::SetVerdict { row_id, .. } = &msg {
        let app_bound_rules = app_bound_rules_for_row(Some(handles), row_id);
        let (limited, changed) =
            crate::pending_decision::limit_to_bridge(msg, app_bound_rules, bindable_process_path);
        if let (
            true,
            snitchwatch_bridge::ws_messages::ClientMessage::SetVerdict { row_id, scope, .. },
        ) = (changed, &limited)
        {
            tracing::warn!(
                %row_id,
                ?scope,
                app_bound_rules,
                bindable_process_path,
                "BridgeFeed: a remembered verdict was sent once-only; the caller should not have asked for it"
            );
        }
        msg = limited;
    }
    // Prompt-slot plan Part C: only a session that advertised it gets a
    // "Decide later", whatever QML offered.
    if let snitchwatch_bridge::ws_messages::ClientMessage::DecideLater { row_id } = &msg {
        if !decide_later_for_row(Some(handles), row_id) {
            tracing::warn!(%row_id, "BridgeFeed: Decide later for a bridge session without it; dropped");
            return Err(crate::bridge_runtime::SendClientMessageError::StaleSession);
        }
    }
    use snitchwatch_bridge::ws_messages::ClientMessage::{DecideLater, SetVerdict};
    if let SetVerdict { row_id, .. } | DecideLater { row_id } = &mut msg {
        let Some((session, wire_id)) = split_session_row_id(row_id) else {
            return Err(crate::bridge_runtime::SendClientMessageError::StaleSession);
        };
        *row_id = wire_id.to_owned();
        handles.try_send_for_session(session, msg)
    } else {
        handles.try_send(msg)
    }
}

/// Send `msg`, which names no row itself (`AddRule` from "Make a rule…"),
/// to the bridge session row `row_id` came from. False without a runtime,
/// for a malformed id, or once that session is gone.
pub(crate) fn dispatch_for_row(
    row_id: &str,
    msg: snitchwatch_bridge::ws_messages::ClientMessage,
) -> bool {
    let Some(handles) = crate::bridge_runtime::handles() else {
        tracing::warn!("BridgeFeed: bridge not running; dropping client message");
        return false;
    };
    let Some((session, _)) = split_session_row_id(row_id) else {
        return false;
    };
    match handles.try_send_for_session(session, msg) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(error = %error, "BridgeFeed: client mutation dropped");
            false
        }
    }
}

/// Whether row `row_id`'s bridge session is live and takes "Decide later".
pub(crate) fn decide_later_for_row(
    handles: Option<&crate::bridge_runtime::BridgeHandles>,
    row_id: &str,
) -> bool {
    match (handles, split_session_row_id(row_id)) {
        (Some(handles), Some((session, _))) => handles.advertises_decide_later(session),
        _ => false,
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

    /// E3: "Make a rule…" on a row the firewall's default action decided. The
    /// row comes from the bridge's own translation of the fork's event; its
    /// local id names its session, and the rule is the program-bound
    /// `AddRule` the bridge accepts.
    #[test]
    fn make_a_rule_for_a_default_decided_event_row_goes_to_its_session() {
        use snitchwatch_bridge::ws_messages::ClientMessage;
        use snitchwatch_proto::protocol::{Connection, Event, Rule};

        assert_eq!(split_session_row_id("1:event-123"), Some((1, "event-123")));
        let event = Event {
            connection: Some(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }),
            rule: Some(Rule {
                description: snitchwatch_bridge::daemon_contract::DEFAULT_ACTION_MARKER.into(),
                action: "deny".into(),
                ..Default::default()
            }),
            unixnano: 123,
            ..Default::default()
        };
        let mut row = snitchwatch_bridge::translator::connection::event_to_row(&event).unwrap();
        assert!(row.decided_by_default);
        row.id = format!("1:{}", row.id);
        let (session, wire_id) = split_session_row_id(&row.id).unwrap();
        assert_eq!(session, 1);
        assert!(wire_id.starts_with("event-123-"), "{wire_id}");

        let msg = crate::make_rule::add_rule_message(
            &row,
            "deny",
            "this_host",
            "forever",
            1_700_000_000_000,
        )
        .expect("a default-decided row gets a rule");
        let ClientMessage::AddRule { rule, .. } = msg else {
            panic!("expected AddRule");
        };
        assert_eq!(rule["action"], "deny");
        let operator = rule["operator"].to_string();
        assert!(
            operator.contains("/usr/bin/curl") && operator.contains("example.com"),
            "{operator}"
        );
    }

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

    fn link(state: LinkState) -> Option<LinkStatus> {
        Some(LinkStatus {
            state,
            detail: "Bridge unavailable: <b>x</b>".to_string(),
        })
    }

    #[test]
    fn every_link_state_is_published_with_its_own_token_and_only_connected_is_ok() {
        for (state, ok, token) in [
            (LinkState::Connecting, false, "connecting"),
            (LinkState::Connected, true, "connected"),
            (LinkState::Retrying, false, "retrying"),
            (LinkState::Failed, false, "failed"),
            (LinkState::Stopped, false, "stopped"),
        ] {
            let (published_ok, detail, published_token) = published(link(state));
            assert_eq!(published_ok, ok, "{state:?}");
            assert_eq!(published_token, token, "{state:?}");
            // The message is carried through untouched, to be shown.
            assert_eq!(detail, "Bridge unavailable: <b>x</b>");
        }
    }

    #[test]
    fn before_the_runtime_starts_the_shell_is_connecting() {
        let (ok, detail, token) = published(None);
        assert!(!ok);
        assert_eq!(token, "connecting");
        assert_eq!(detail, "Bridge not started");
    }
}
