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
//! "This time", "For 5 minutes", "Until firewall restarts", "Forever" —
//! mapped onto the bridge's [`VerdictDuration`], which in turn maps onto
//! opensnitchd's native `Rule.duration` vocabulary. See [`VerdictDuration`]'s
//! doc comment for the full mapping table. The third option's QML token is
//! still `until_quit` (daemon "until restart"); it is labelled for what the
//! daemon does, since opensnitchd has no per-process rule lifetime.
//!
//! **Inline buttons:** the Connections page's row and process-header buttons
//! skip the sheet; `crate::inline_deny` picks their duration.
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
//!
//! **Last gate:** `dispatch_to` runs every verdict through [`limit_to_bridge`],
//! which sends a remembered one once-only when the caller reports no bindable
//! program or the bridge session can't bind a host scope to it (issues #44,
//! #72).

use snitchwatch_bridge::ws_messages::{
    effective_verdict_duration, ClientMessage, VerdictAction, VerdictDuration, VerdictScope,
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

/// Issues #44 and #72: whether a verdict may be remembered, given what the
/// caller reports about its program and what the row's bridge session can do.
/// `bindable_process_path` is the caller's word that the program has a file a
/// rule can be bound to (`is_bindable_process_path`; the feed can't look the
/// row up itself). A bridge session that didn't advertise app-bound rules
/// (`bridge_capabilities::APP_BOUND_RULES`) builds "This host only" and "Any
/// host on this domain" rules without the program, so those would cover every
/// app. "Any host" matches the program alone, even on those bridges.
fn may_remember(scope: VerdictScope, app_bound_rules: bool, bindable_process_path: bool) -> bool {
    bindable_process_path && (app_bound_rules || matches!(scope, VerdictScope::AnyHost))
}

/// Send a verdict that asks to be remembered once-only instead, unless
/// [`may_remember`]. Returns the message to send and whether it changed, so
/// the caller can say so. A verdict already once-only, and any other message,
/// is returned as it came.
///
/// This is only as good as `bindable_process_path`: it does not look at the
/// row itself (see `BridgeFeed::submitVerdict`).
pub(crate) fn limit_to_bridge(
    msg: ClientMessage,
    app_bound_rules: bool,
    bindable_process_path: bool,
) -> (ClientMessage, bool) {
    match msg {
        ClientMessage::SetVerdict {
            row_id,
            verdict,
            scope,
            duration,
            remember,
        } if effective_verdict_duration(duration, remember).remembers()
            && !may_remember(scope, app_bound_rules, bindable_process_path) =>
        {
            let limited = ClientMessage::SetVerdict {
                row_id,
                verdict,
                scope,
                duration: Some(VerdictDuration::Once),
                remember: None,
            };
            (limited, true)
        }
        other => (other, false),
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

    fn verdict(
        scope: VerdictScope,
        duration: Option<VerdictDuration>,
        remember: Option<bool>,
    ) -> ClientMessage {
        ClientMessage::SetVerdict {
            row_id: "1:7".to_string(),
            verdict: VerdictAction::Allow,
            scope,
            duration,
            remember,
        }
    }

    /// Every way a verdict can ask to be remembered, the legacy pre-duration
    /// `remember` included.
    const REMEMBERED: [(Option<VerdictDuration>, Option<bool>); 4] = [
        (Some(VerdictDuration::FiveMinutes), None),
        (Some(VerdictDuration::UntilRestart), None),
        (Some(VerdictDuration::Always), None),
        (None, Some(true)),
    ];
    const SCOPES: [VerdictScope; 3] = [
        VerdictScope::ThisHost,
        VerdictScope::AnyHostOnDomain,
        VerdictScope::AnyHost,
    ];

    fn once(scope: VerdictScope) -> ClientMessage {
        verdict(scope, Some(VerdictDuration::Once), None)
    }

    #[test]
    fn an_old_bridge_never_remembers_a_host_scoped_verdict() {
        for scope in [VerdictScope::ThisHost, VerdictScope::AnyHostOnDomain] {
            for (duration, remember) in REMEMBERED {
                assert_eq!(
                    limit_to_bridge(verdict(scope, duration, remember), false, true),
                    (once(scope), true),
                    "{scope:?} {duration:?} {remember:?}"
                );
            }
        }
    }

    #[test]
    fn an_unidentifiable_program_is_never_remembered_under_any_scope_on_any_bridge() {
        // "Any host" included: on an old bridge an empty process path turns
        // into a host-only rule for every app (#44).
        for app_bound_rules in [false, true] {
            for scope in SCOPES {
                for (duration, remember) in REMEMBERED {
                    assert_eq!(
                        limit_to_bridge(verdict(scope, duration, remember), app_bound_rules, false),
                        (once(scope), true),
                        "{scope:?} {duration:?} {remember:?} app-bound {app_bound_rules}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_verdict_the_bridge_can_bind_is_kept_and_not_reported() {
        let kept = [
            // "Any host" is the program alone, so an old bridge keeps it.
            (
                verdict(VerdictScope::AnyHost, Some(VerdictDuration::Always), None),
                false,
                true,
            ),
            (
                verdict(VerdictScope::ThisHost, Some(VerdictDuration::Always), None),
                true,
                true,
            ),
            (
                verdict(VerdictScope::AnyHostOnDomain, None, Some(true)),
                true,
                true,
            ),
            // Already once-only, in either shape: nothing to downgrade.
            (once(VerdictScope::ThisHost), false, true),
            (once(VerdictScope::AnyHost), false, false),
            (
                verdict(VerdictScope::ThisHost, None, Some(false)),
                false,
                false,
            ),
            (verdict(VerdictScope::ThisHost, None, None), false, false),
            (ClientMessage::RequestSnapshot, false, false),
        ];
        for (msg, app_bound_rules, bindable) in kept {
            assert_eq!(
                limit_to_bridge(msg.clone(), app_bound_rules, bindable),
                (msg, false)
            );
        }
    }

    #[test]
    fn build_message_for_5_minute_duration_serializes_the_wire_token() {
        let msg = build_verdict_message("r9", "allow", "this_host", "for_5_minutes").unwrap();
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["duration"], "five_minutes");
    }
}
