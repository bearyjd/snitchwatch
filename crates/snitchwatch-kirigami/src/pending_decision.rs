//! The pending-`AskRule` decision surface (Task 7; Parity 2 duration scopes).
//!
//! This is the single most safety-critical interaction in the app: the user
//! allows or denies a novel connection. Mapping the two verdict buttons
//! (allow/deny), the host-match scope selector, and the duration selector onto
//! the bridge's typed [`ClientMessage::SetVerdict`] lives here as Qt-free,
//! unit-tested functions with no QObject of their own. QML reaches them
//! through `BridgeFeed::submitVerdict`, which builds the message here and
//! hands it straight to the bridge's inbound pump.
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
//! **Live wiring:** `BridgeFeed::submitVerdict` calls
//! [`build_verdict_message`] and dispatches the result onto the bridge's
//! inbound channel, which resolves the pending `AskRule`'s
//! `oneshot::Sender<Verdict>`. No bridge code changes were needed for the
//! scope/duration extension — it only builds the existing `SetVerdict`
//! message the WS protocol already defines (now carrying a typed `duration`
//! field instead of a plain `remember: bool`).

use snitchwatch_bridge::ws_messages::{
    ClientMessage, VerdictAction, VerdictDuration, VerdictScope,
};

/// The two verdict buttons on the decision sheet. The once/always axis that
/// used to live on this enum is now the independent duration selector (see
/// [`parse_duration`]) — Little-Snitch-parity durations aren't just binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerdictChoice {
    Allow,
    Deny,
}

impl VerdictChoice {
    /// Parse the stable lowercase token QML sends.
    pub(crate) fn from_token(token: &str) -> Option<Self> {
        match token {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// The underlying allow/deny action.
    pub(crate) fn action(self) -> VerdictAction {
        match self {
            Self::Allow => VerdictAction::Allow,
            Self::Deny => VerdictAction::Deny,
        }
    }
}

/// Parse the scope token QML sends into the bridge's [`VerdictScope`].
/// Defaults to the most conservative scope ([`VerdictScope::ThisHost`]) for an
/// unrecognised token rather than widening the rule unexpectedly.
pub(crate) fn parse_scope(token: &str) -> VerdictScope {
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
pub(crate) fn parse_duration(token: &str) -> VerdictDuration {
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
        duration: Some(parse_duration(duration_token)),
        remember: None,
    })
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
                remember,
            } => {
                assert_eq!(row_id, "r1");
                assert_eq!(verdict, VerdictAction::Deny);
                assert_eq!(scope, VerdictScope::AnyHost);
                assert_eq!(duration, Some(VerdictDuration::Always));
                assert_eq!(remember, None);
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
