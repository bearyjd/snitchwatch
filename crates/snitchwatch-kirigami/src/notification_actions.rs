//! Answering a waiting prompt from its desktop notification (prompt-slot
//! plan Part B; owner decision S5: "Allow once" and "Deny" only, so a
//! remembered Allow still needs the window).
//!
//! Each action does what the Connections page's inline button does
//! (`InlineVerdicts.qml`): Allow is once only; Deny is remembered until the
//! firewall restarts, bound to the program and this host, only for a program
//! file the bridge can name on a session that advertised app-bound rules
//! (`crate::inline_deny`), and otherwise once only, with the plain reason.
//! It goes through `bridge_feed::dispatch_to`, so #77's gate and the
//! session-routed send apply too.
//!
//! A notification can outlive its prompt: the row may be answered elsewhere,
//! put off, or belong to a session that has gone. So a notice is shown, and
//! an action taken, only while the row still waits in the same session
//! (`BridgeHandles::pending_row`). Otherwise nothing is sent and the user is
//! told so.

use snitchwatch_bridge::translator::process_binding::{is_bindable_process_path, RuleRefusal};
use snitchwatch_bridge::translator::verdict::sanitize_for_display;

use crate::bridge_runtime::{BridgeHandles, PendingRow};
use crate::inline_deny::InlineDeny;
use crate::pending_decision::build_verdict_message;

/// D-Bus action ids for the pending notification's answers.
pub(crate) const ALLOW_ONCE_ACTION: &str = "allow-once";
pub(crate) const DENY_ACTION: &str = "deny";

/// The pending notification's body. The program and host come from outside
/// and freedesktop notification servers render a markup subset in bodies,
/// so both are escaped (and stripped of control and bidi characters) by the
/// bridge's display sanitizer.
pub(crate) fn pending_body(row: &PendingRow) -> String {
    format!(
        "{} wants to connect to {}",
        sanitize_for_display(&row.process, 64),
        sanitize_for_display(&row.dst_host, 128)
    )
}

/// An answer offered on the pending notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    AllowOnce,
    Deny,
}

impl NoticeAction {
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            ALLOW_ONCE_ACTION => Some(Self::AllowOnce),
            DENY_ACTION => Some(Self::Deny),
            _ => None,
        }
    }
}

/// What an action did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionOutcome {
    /// Sent, as the inline button would have.
    Sent,
    /// A Deny sent for this connection only, and why.
    DeniedOnce(InlineDeny),
    /// The row isn't waiting in this session any more; nothing was sent.
    NoLongerWaiting,
    /// The answer couldn't be queued: the bridge connection is gone.
    NotSent,
}

/// `InlineVerdicts.qml`'s `bridgeTooOldSentence` (a test keeps them equal).
pub(crate) const BRIDGE_TOO_OLD: &str =
    "This firewall bridge is too old to block just this program, so Deny applies to this connection only.";
pub(crate) const NO_LONGER_WAITING: &str =
    "That connection is no longer waiting for an answer, so nothing was sent.";
pub(crate) const NOT_SENT: &str =
    "The connection to the background service was lost, so the answer wasn't sent.";

impl ActionOutcome {
    /// The fixed text of a follow-up notification, when one is needed.
    pub(crate) fn explanation(self) -> Option<&'static str> {
        match self {
            Self::Sent | Self::DeniedOnce(InlineDeny::UntilRestart) => None,
            Self::DeniedOnce(InlineDeny::ProgramUnknown) => {
                Some(RuleRefusal::ProcessFileUnknown.describe())
            }
            Self::DeniedOnce(InlineDeny::BridgeTooOld) => Some(BRIDGE_TOO_OLD),
            Self::NoLongerWaiting => Some(NO_LONGER_WAITING),
            Self::NotSent => Some(NOT_SENT),
        }
    }
}

/// A pending `notice`'s row wire id and row, while it still waits in
/// session `connection_id`; `None` for any other notice. Asked when the
/// grace period ends, so a prompt answered, put off or withdrawn meanwhile
/// (a tray-only pause, say) is never announced.
pub(crate) fn still_waiting(
    handles: &BridgeHandles,
    connection_id: u64,
    notice: &crate::bridge_runtime::BridgeNotice,
) -> Option<(String, PendingRow)> {
    let crate::bridge_runtime::BridgeNotice::Pending { row_id, .. } = notice else {
        return None;
    };
    let wire_id = snitchwatch_bridge::translator::connection::ask_row_id(*row_id);
    let row = handles.pending_row(connection_id, &wire_id)?;
    Some((wire_id, row))
}

/// Answer row `wire_id` of session `connection_id` with `action`.
pub(crate) fn act(
    handles: &BridgeHandles,
    connection_id: u64,
    wire_id: &str,
    action: NoticeAction,
) -> ActionOutcome {
    let Some(row) = handles.pending_row(connection_id, wire_id) else {
        return ActionOutcome::NoLongerWaiting;
    };
    let bindable = row
        .process_path
        .as_deref()
        .is_some_and(is_bindable_process_path);
    let (choice, deny) = match action {
        NoticeAction::AllowOnce => ("allow", None),
        NoticeAction::Deny => (
            "deny",
            Some(InlineDeny::decide(
                row.process_path.as_deref(),
                handles.advertises_app_bound_rules(connection_id),
            )),
        ),
    };
    let duration = deny.map_or("this_time", InlineDeny::duration_token);
    let row_id = format!("{connection_id}:{wire_id}");
    let Some(msg) = build_verdict_message(&row_id, choice, "this_host", duration) else {
        return ActionOutcome::NotSent;
    };
    if let Err(error) = crate::bridge_feed::dispatch_to(handles, msg, bindable) {
        tracing::warn!(%error, %row_id, "notification answer not sent");
        return ActionOutcome::NotSent;
    }
    match deny {
        Some(reason) if reason != InlineDeny::UntilRestart => ActionOutcome::DeniedOnce(reason),
        _ => ActionOutcome::Sent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(process: &str, dst_host: &str) -> PendingRow {
        PendingRow {
            process: process.into(),
            process_path: Some(format!("/usr/bin/{process}")),
            dst_host: dst_host.into(),
        }
    }

    #[test]
    fn the_body_never_carries_markup_from_the_program_or_host() {
        assert_eq!(
            pending_body(&row("curl", "example.com")),
            "curl wants to connect to example.com"
        );
        let body = pending_body(&row(
            "<b>evil</b>",
            "<img src=\"http://tracker.example/x.png\">&amp;\u{202e}moc.knab",
        ));
        assert!(!body.contains('<') && !body.contains('>'), "{body}");
        assert!(body.contains("&lt;b&gt;evil&lt;/b&gt;"), "{body}");
        assert!(body.contains("&amp;amp;"), "{body}");
        assert!(!body.contains('\u{202e}'), "bidi override kept: {body}");
    }

    #[test]
    fn only_allow_once_and_deny_are_answers() {
        assert_eq!(
            NoticeAction::from_id("allow-once"),
            Some(NoticeAction::AllowOnce)
        );
        assert_eq!(NoticeAction::from_id("deny"), Some(NoticeAction::Deny));
        for other in ["review", "allow", "allow-forever", ""] {
            assert_eq!(NoticeAction::from_id(other), None, "{other:?}");
        }
    }

    #[test]
    fn every_outcome_that_needs_words_has_plain_fixed_text() {
        assert_eq!(ActionOutcome::Sent.explanation(), None);
        assert_eq!(
            ActionOutcome::DeniedOnce(InlineDeny::UntilRestart).explanation(),
            None
        );
        assert_eq!(
            ActionOutcome::DeniedOnce(InlineDeny::ProgramUnknown).explanation(),
            Some(RuleRefusal::ProcessFileUnknown.describe())
        );
        assert_eq!(
            ActionOutcome::DeniedOnce(InlineDeny::BridgeTooOld).explanation(),
            Some(BRIDGE_TOO_OLD)
        );
        assert_eq!(
            ActionOutcome::NoLongerWaiting.explanation(),
            Some(NO_LONGER_WAITING)
        );
        assert_eq!(ActionOutcome::NotSent.explanation(), Some(NOT_SENT));
    }

    #[test]
    fn the_too_old_sentence_matches_the_inline_one() {
        let inline = include_str!("../qml/InlineVerdicts.qml");
        assert!(
            inline.contains(BRIDGE_TOO_OLD),
            "InlineVerdicts.qml drifted"
        );
    }
}
