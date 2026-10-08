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

use snitchwatch_bridge::translator::display::{
    sanitize_ends_for_display, sanitize_tail_for_display,
};
use snitchwatch_bridge::translator::process_binding::{is_bindable_process_path, RuleRefusal};
use snitchwatch_bridge::ws_messages::{ConnectionRow, ServerMessage};
use tokio::sync::broadcast;

use crate::bridge_runtime::{BridgeHandles, PendingRow, ReceivedServerMessage};
use crate::connections::outcome::is_pending;
use crate::inline_deny::InlineDeny;
use crate::pending_decision::build_verdict_message;

/// D-Bus action ids for the pending notification's answers.
pub(crate) const ALLOW_ONCE_ACTION: &str = "allow-once";
pub(crate) const DENY_ACTION: &str = "deny";
pub(crate) const REVIEW_ACTION: &str = "review";

/// How much of a program path or host a notification shows. A longer path
/// keeps its start and its end, with "…" between; a longer host keeps its
/// end, behind a leading "…".
const BODY_PATH_CHARS: usize = 120;
const BODY_HOST_CHARS: usize = 100;

/// The pending notification's body. The program and host come from outside
/// and freedesktop notification servers render a markup subset in bodies,
/// so both are escaped (and stripped of control and format characters) by
/// the bridge's display sanitizer.
///
/// It shows the program's full path, not just its file name: a
/// `/tmp/x/firefox` must not read as "firefox". A long host keeps its end,
/// so `login.microsoft.com.<padding>.evil.tld` still shows the real domain
/// (PR #100 review). A long path keeps its start too, so a padded
/// `/tmp/x/<padding>/usr/lib64/firefox/firefox` still shows it is in /tmp
/// (PR #100 re-review).
///
/// When Deny would be remembered (`deny`, from `InlineDeny::decide`), the
/// body says for how long, as the inline Deny's tooltip does; a once-only
/// Deny explains itself after the click.
pub(crate) fn pending_body(row: &PendingRow, deny: InlineDeny) -> String {
    let program = row.process_path.as_deref().unwrap_or(&row.process);
    let asking = format!(
        "{} wants to connect to {}",
        sanitize_ends_for_display(program, BODY_PATH_CHARS),
        sanitize_tail_for_display(&row.dst_host, BODY_HOST_CHARS)
    );
    match deny {
        InlineDeny::UntilRestart => format!("{asking}\n{DENY_UNTIL_RESTART}"),
        InlineDeny::ProgramUnknown | InlineDeny::BridgeTooOld => asking,
    }
}

/// What a remembered Deny does; the inline Deny's tooltip says the same.
pub(crate) const DENY_UNTIL_RESTART: &str =
    "Deny blocks this program from this host until the firewall restarts.";

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
    /// It was answered another way first (in the window, by a pause, by the
    /// timeout), so the bridge didn't use this answer.
    AlreadyAnswered,
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
pub(crate) const ALREADY_ANSWERED: &str =
    "This prompt was already answered, so your answer wasn't used.";

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
            Self::AlreadyAnswered => Some(ALREADY_ANSWERED),
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

/// How long [`act_and_confirm`] listens for the bridge's row update.
pub(crate) const CONFIRM_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// [`act`], then a check of what the bridge did with it. The bridge answers
/// a verdict for a row that stopped waiting by doing nothing, so this
/// watches for the row's update instead. It subscribes before `act` looks,
/// so no update is missed:
/// - the row decided as we answered: the outcome stands;
/// - decided another way (another answer, a pause, the timeout): the
///   prompt was already answered;
/// - withdrawn: it isn't waiting any more;
/// - nothing within [`CONFIRM_WAIT`]: the outcome stands, as nothing more
///   is known.
pub(crate) async fn act_and_confirm(
    handles: &BridgeHandles,
    connection_id: u64,
    wire_id: &str,
    action: NoticeAction,
) -> ActionOutcome {
    let mut updates = handles.subscribe();
    let outcome = act(handles, connection_id, wire_id, action);
    if !matches!(outcome, ActionOutcome::Sent | ActionOutcome::DeniedOnce(_)) {
        return outcome;
    }
    let ours = match action {
        NoticeAction::AllowOnce => "allow",
        NoticeAction::Deny => "deny",
    };
    let settled = tokio::time::timeout(
        CONFIRM_WAIT,
        row_settled(&mut updates, connection_id, wire_id),
    )
    .await;
    match settled {
        Ok(Some(Some(row)))
            if row.action.as_deref() == Some(ours)
                && !row.deferred
                && row.auto_answer.is_none() =>
        {
            outcome
        }
        Ok(Some(Some(_))) => ActionOutcome::AlreadyAnswered,
        Ok(Some(None)) => ActionOutcome::NoLongerWaiting,
        Ok(None) | Err(_) => outcome,
    }
}

/// The next settling of row `wire_id` in session `connection_id`:
/// `Some(Some(row))` when it is decided or put off, `Some(None)` when it is
/// withdrawn, `None` when the feed ends.
async fn row_settled(
    updates: &mut broadcast::Receiver<ReceivedServerMessage>,
    connection_id: u64,
    wire_id: &str,
) -> Option<Option<ConnectionRow>> {
    loop {
        let received = match updates.recv().await {
            Ok(received) => received,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        if received.connection_id != connection_id {
            continue;
        }
        match received.message {
            ServerMessage::InsertConnectionRows { rows }
            | ServerMessage::UpdateConnectionRows { rows } => {
                if let Some(row) = rows
                    .into_iter()
                    .find(|row| row.id == wire_id && !is_pending(row))
                {
                    return Some(Some(row));
                }
            }
            ServerMessage::RemoveConnectionRows { ids }
            | ServerMessage::MoveConnetionRows { ids }
                if ids.iter().any(|id| id == wire_id) =>
            {
                return Some(None);
            }
            ServerMessage::ClearConnectionRows => return Some(None),
            _ => {}
        }
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
            pending_body(&row("curl", "example.com"), InlineDeny::ProgramUnknown),
            "/usr/bin/curl wants to connect to example.com"
        );
        let body = pending_body(
            &row(
                "<b>evil</b>",
                "<img src=\"http://tracker.example/x.png\">&amp;\u{202e}moc.knab",
            ),
            InlineDeny::UntilRestart,
        );
        assert!(!body.contains('<') && !body.contains('>'), "{body}");
        assert!(body.contains("&lt;b&gt;evil&lt;/b&gt;"), "{body}");
        assert!(body.contains("&amp;amp;"), "{body}");
        assert!(!body.contains('\u{202e}'), "bidi override kept: {body}");
    }

    #[test]
    fn the_body_shows_the_whole_program_path_and_the_end_of_a_long_host() {
        let spoof = PendingRow {
            process: "firefox".into(),
            process_path: Some("/tmp/x/firefox".into()),
            dst_host: format!("login.microsoft.com.{}.evil.tld", "a".repeat(300)),
        };
        let body = pending_body(&spoof, InlineDeny::ProgramUnknown);
        assert!(
            body.starts_with("/tmp/x/firefox wants to connect to …"),
            "{body}"
        );
        assert!(body.ends_with(".evil.tld"), "{body}");
        // A long path keeps its start as well as its end, so a padded
        // `/tmp/x/…/usr/lib64/firefox/firefox` still shows it is in /tmp.
        let long_path = PendingRow {
            process_path: Some(format!(
                "/tmp/x/{}/usr/lib64/firefox/firefox",
                "d/".repeat(200)
            )),
            ..spoof.clone()
        };
        let body = pending_body(&long_path, InlineDeny::ProgramUnknown);
        assert!(body.starts_with("/tmp/x/d/"), "{body}");
        assert!(
            body.contains("…") && body.contains("/usr/lib64/firefox/firefox wants to connect to"),
            "{body}"
        );
        // With no path, the program name stands in.
        let nameless = PendingRow {
            process_path: None,
            dst_host: "example.com".into(),
            ..spoof
        };
        assert_eq!(
            pending_body(&nameless, InlineDeny::ProgramUnknown),
            "firefox wants to connect to example.com"
        );
    }

    #[test]
    fn the_body_says_how_long_a_remembered_deny_lasts() {
        let body = |deny| pending_body(&row("curl", "example.com"), deny);
        assert_eq!(
            body(InlineDeny::UntilRestart),
            format!("/usr/bin/curl wants to connect to example.com\n{DENY_UNTIL_RESTART}")
        );
        for once in [InlineDeny::ProgramUnknown, InlineDeny::BridgeTooOld] {
            assert_eq!(body(once), "/usr/bin/curl wants to connect to example.com");
        }
        let inline = include_str!("../qml/InlineVerdicts.qml");
        assert!(
            inline.contains("Blocks this program from this host until the firewall restarts")
                && DENY_UNTIL_RESTART
                    .contains("blocks this program from this host until the firewall restarts"),
            "the notification and the inline Deny drifted"
        );
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
        assert_eq!(
            ActionOutcome::AlreadyAnswered.explanation(),
            Some(ALREADY_ANSWERED)
        );
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
