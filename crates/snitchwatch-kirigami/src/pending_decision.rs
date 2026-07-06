//! The pending-`AskRule` decision surface (Task 7; Parity 2 duration scopes).
//!
//! This is the single most safety-critical interaction in the app: the user
//! allows or denies a novel connection. The pure part of that decision —
//! mapping the two verdict buttons (allow/deny), the host-match scope
//! selector, and the duration selector onto the bridge's typed
//! [`ClientMessage::SetVerdict`] — lives here as a Qt-free, unit-tested
//! function. The cxx-qt `PendingDecision` wrapper is a thin surface QML
//! calls; it serialises the message and emits it via a signal.
//!
//! **Granular rule scopes (Parity 2):** the dialog offers four durations —
//! "This time", "For 5 minutes", "Until quit", "Forever" — mapped onto the
//! bridge's [`VerdictDuration`], which in turn maps onto opensnitchd's native
//! `Rule.duration` vocabulary. See [`VerdictDuration`]'s doc comment for the
//! full mapping table, including the one lossy case ("Until quit" -> daemon
//! "until restart", since opensnitchd has no per-process rule lifetime).
//!
//! **Timeout ownership:** the auto-action countdown stays server-side (the
//! bridge's `AskRule` pending machinery owns it). The QML sheet only *displays*
//! remaining time via a `remainingSeconds` property the bridge feed sets; this
//! module never starts a client-side timer.
//!
//! **Live wiring (follow-up):** `verdictSubmitted(json)` is emitted for each
//! decision. The bridge feed (the same consumer-side wiring `ConnectionsModel`
//! awaits) connects it to the bridge's existing `oneshot::Sender<Verdict>`
//! resolution path. No bridge code changes were needed for the scope/duration
//! extension — it only builds the existing `SetVerdict` message the WS
//! protocol already defines (now carrying a typed `duration` field instead of
//! a plain `remember: bool`).

use core::pin::Pin;
use cxx_qt_lib::QString;

use snitchwatch_bridge::ws_messages::{
    ClientMessage, VerdictAction, VerdictDuration, VerdictScope,
};

/// The two verdict buttons on the decision sheet. The once/always axis that
/// used to live on this enum is now the independent duration selector (see
/// [`parse_duration`]) — Little-Snitch-parity durations aren't just binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictChoice {
    Allow,
    Deny,
}

impl VerdictChoice {
    /// Parse the stable lowercase token QML sends.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// The underlying allow/deny action.
    pub fn action(self) -> VerdictAction {
        match self {
            Self::Allow => VerdictAction::Allow,
            Self::Deny => VerdictAction::Deny,
        }
    }
}

/// Parse the scope token QML sends into the bridge's [`VerdictScope`].
/// Defaults to the most conservative scope ([`VerdictScope::ThisHost`]) for an
/// unrecognised token rather than widening the rule unexpectedly.
pub fn parse_scope(token: &str) -> VerdictScope {
    match token {
        "any_host_on_domain" => VerdictScope::AnyHostOnDomain,
        "any_host" => VerdictScope::AnyHost,
        _ => VerdictScope::ThisHost,
    }
}

/// Parse the duration token QML sends into the bridge's [`VerdictDuration`].
/// Defaults to the most conservative option ([`VerdictDuration::Once`]) for an
/// unrecognised token — a UI bug must never silently create a persistent
/// rule.
///
/// | QML token         | [`VerdictDuration`]            |
/// |-------------------|---------------------------------|
/// | `this_time`       | [`VerdictDuration::Once`]        |
/// | `for_5_minutes`   | [`VerdictDuration::FiveMinutes`] |
/// | `until_quit`      | [`VerdictDuration::UntilRestart`]|
/// | `forever`         | [`VerdictDuration::Always`]      |
/// | anything else     | [`VerdictDuration::Once`] (safe default) |
pub fn parse_duration(token: &str) -> VerdictDuration {
    match token {
        "for_5_minutes" => VerdictDuration::FiveMinutes,
        "until_quit" => VerdictDuration::UntilRestart,
        "forever" => VerdictDuration::Always,
        _ => VerdictDuration::Once,
    }
}

/// Build the typed `SetVerdict` client message for a decision. Returns `None`
/// only when the choice token is unrecognised (a programming error in the QML,
/// surfaced rather than silently sending a wrong verdict).
pub fn build_verdict_message(
    row_id: &str,
    choice_token: &str,
    scope_token: &str,
    duration_token: &str,
) -> Option<ClientMessage> {
    let choice = VerdictChoice::from_token(choice_token)?;
    Some(ClientMessage::SetVerdict {
        row_id: row_id.to_string(),
        verdict: choice.action(),
        scope: parse_scope(scope_token),
        duration: parse_duration(duration_token),
    })
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Verdict submission surface bound by `PendingDecisionSheet.qml`.
        #[qobject]
        #[qml_element]
        type PendingDecision = super::PendingDecisionRust;

        /// Emitted with the JSON-encoded `ClientMessage::SetVerdict` for each
        /// decision. The live bridge feed connects this to the bridge's verdict
        /// resolution path (no bridge changes — same message the WS protocol
        /// already carries).
        #[qsignal]
        #[cxx_name = "verdictSubmitted"]
        fn verdict_submitted(self: Pin<&mut PendingDecision>, json: QString);

        /// Submit a decision. `choice` is `allow` / `deny`; `scope` is
        /// `this_host` / `any_host_on_domain` / `any_host`; `duration` is
        /// `this_time` / `for_5_minutes` / `until_quit` / `forever`.
        /// Malformed input is logged and dropped (never sends a wrong
        /// verdict).
        #[qinvokable]
        fn submit(
            self: Pin<&mut PendingDecision>,
            row_id: &QString,
            choice: &QString,
            scope: &QString,
            duration: &QString,
        );
    }
}

/// Rust-side state for [`qobject::PendingDecision`] (stateless — the timeout is
/// server-side, so this holds nothing).
#[derive(Default)]
pub struct PendingDecisionRust;

impl qobject::PendingDecision {
    fn submit(
        self: Pin<&mut Self>,
        row_id: &QString,
        choice: &QString,
        scope: &QString,
        duration: &QString,
    ) {
        let row_id = row_id.to_string();
        let choice = choice.to_string();
        let scope = scope.to_string();
        let duration = duration.to_string();
        match build_verdict_message(&row_id, &choice, &scope, &duration) {
            Some(msg) => match serde_json::to_string(&msg) {
                Ok(json) => self.verdict_submitted(QString::from(&json)),
                Err(e) => tracing::error!(error = %e, "PendingDecision: verdict serialize failed"),
            },
            None => {
                tracing::warn!(%choice, "PendingDecision: unrecognised verdict choice, ignored")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_tokens_map_to_action() {
        assert_eq!(
            VerdictChoice::from_token("allow"),
            Some(VerdictChoice::Allow)
        );
        assert_eq!(VerdictChoice::Allow.action(), VerdictAction::Allow);

        assert_eq!(VerdictChoice::from_token("deny"), Some(VerdictChoice::Deny));
        assert_eq!(VerdictChoice::Deny.action(), VerdictAction::Deny);

        assert_eq!(VerdictChoice::from_token("garbage"), None);
    }

    #[test]
    fn scope_parsing_defaults_to_this_host_conservatively() {
        assert_eq!(parse_scope("this_host"), VerdictScope::ThisHost);
        assert_eq!(
            parse_scope("any_host_on_domain"),
            VerdictScope::AnyHostOnDomain
        );
        assert_eq!(parse_scope("any_host"), VerdictScope::AnyHost);
        // Unknown token must NOT widen the rule.
        assert_eq!(parse_scope("weird"), VerdictScope::ThisHost);
    }

    #[test]
    fn duration_tokens_map_to_the_documented_table() {
        assert_eq!(parse_duration("this_time"), VerdictDuration::Once);
        assert_eq!(
            parse_duration("for_5_minutes"),
            VerdictDuration::FiveMinutes
        );
        assert_eq!(parse_duration("until_quit"), VerdictDuration::UntilRestart);
        assert_eq!(parse_duration("forever"), VerdictDuration::Always);
    }

    #[test]
    fn duration_parsing_defaults_to_once_conservatively() {
        // Unknown token must NOT silently create a persistent rule.
        assert_eq!(
            parse_duration("literally anything else"),
            VerdictDuration::Once
        );
        assert_eq!(parse_duration(""), VerdictDuration::Once);
    }

    #[test]
    fn build_message_produces_expected_set_verdict() {
        let msg = build_verdict_message("r1", "deny", "any_host", "forever").unwrap();
        match msg {
            ClientMessage::SetVerdict {
                row_id,
                verdict,
                scope,
                duration,
            } => {
                assert_eq!(row_id, "r1");
                assert_eq!(verdict, VerdictAction::Deny);
                assert_eq!(scope, VerdictScope::AnyHost);
                assert_eq!(duration, VerdictDuration::Always);
            }
            other => panic!("expected SetVerdict, got {other:?}"),
        }
    }

    #[test]
    fn build_message_rejects_unknown_choice() {
        assert!(build_verdict_message("r1", "maybe", "this_host", "this_time").is_none());
    }

    #[test]
    fn build_message_serializes_to_expected_json() {
        let msg = build_verdict_message("r9", "allow", "this_host", "this_time").unwrap();
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setVerdict");
        assert_eq!(json["rowId"], "r9");
        assert_eq!(json["verdict"], "allow");
        assert_eq!(json["scope"], "this_host");
        assert_eq!(json["duration"], "once");
    }

    #[test]
    fn build_message_for_5_minute_duration_serializes_the_wire_token() {
        let msg = build_verdict_message("r9", "allow", "this_host", "for_5_minutes").unwrap();
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["duration"], "five_minutes");
    }
}
