//! The rule editor's messages and texts (roadmap P2.1), without Qt:
//! what is sent, when saving is allowed, what each result says, and
//! whether a cached rule can be edited at all. `crate::rule_editor_controller`
//! binds it to QML. Every string is plain text.

use serde::Serialize;
use serde_json::Value;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome, ServerMessage};
use std::time::{Duration, Instant};

use super::editor::{plain_problems, EditorCheck, RuleDraft};
use super::row_store::RulesStore;

/// Starts every reason Edit is unavailable.
pub const NOT_EDITABLE: &str = "Edit isn't available for this rule: ";
pub const FIX_FIRST: &str = "Fix the problems listed above first.";
pub const CONFIRM_CAUTIONS: &str =
    "This change loosens the rule. Read the cautions above, then choose Save anyway.";
const SAVED: &str = "Saved.";
const NOT_SAVED: &str = "Not saved: ";
const NOT_SENT: &str = "Not sent: ";
const NO_DAEMON: &str = "The firewall service isn't connected, so nothing was sent.";
/// No answer: the change may or may not have been made.
pub const UNKNOWN: &str = "No answer from the firewall in time. The change may have been \
     saved; check the Rules page.";

/// A cached rule for the editor: its wire form, and why it can't be edited
/// (empty when it can).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Editable {
    pub rule: Value,
    pub not_editable: String,
}

/// Whether the editor can change `rule` (a rule from the bridge's list):
/// not when the bridge marks it read-only, nor when the editor can't
/// express it (`RuleDraft::from_wire`). Such rules keep toggle and delete.
pub fn editable(rule: Value, read_only_reason: Option<&str>) -> Editable {
    let not_editable = match read_only_reason {
        Some(reason) => format!("{NOT_EDITABLE}{reason}"),
        None => match RuleDraft::from_wire(&rule) {
            Ok(_) => String::new(),
            Err(reason) => format!("{NOT_EDITABLE}{reason}"),
        },
    };
    Editable { rule, not_editable }
}

/// [`editable`] for the cached rule `name`, in its wire form (the fields
/// the bridge sent, without its display-only ones).
pub fn editable_in(store: &RulesStore, name: &str) -> Option<Editable> {
    let rule = store.find_by_name(name)?;
    let wire = serde_json::to_value(rule).ok()?;
    Some(editable(wire, rule.read_only_reason.as_deref()))
}

/// `AddRule` for a new rule; `UpdateRule` naming the rule being edited
/// (`editing`) otherwise, which the bridge treats as a rename when the
/// draft's name differs.
pub fn submit_message(draft: &RuleDraft, editing: &str, request_id: String) -> ClientMessage {
    let rule = draft.to_wire();
    if editing.is_empty() {
        ClientMessage::AddRule {
            rule,
            request_id: Some(request_id),
            reply: None,
        }
    } else {
        ClientMessage::UpdateRule {
            rule_id: editing.to_string(),
            rule,
            request_id: Some(request_id),
            reply: None,
        }
    }
}

/// Whether a checked draft may be sent: never with problems, and with
/// cautions only once the user confirmed them.
pub fn may_submit(check: &EditorCheck, confirmed: bool) -> Result<(), &'static str> {
    if !check.problems.is_empty() {
        return Err(FIX_FIRST);
    }
    if !check.cautions.is_empty() && !confirmed {
        return Err(CONFIRM_CAUTIONS);
    }
    Ok(())
}

/// A finished command: whether the sheet can close, and what to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub saved: bool,
    pub status: String,
}

/// What a rule command's result says, per sheet (the editor's
/// [`EDITOR_WORDING`], "Make a rule…"'s own). Plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wording {
    /// Ok.
    pub saved: &'static str,
    /// Starts a rejection's reason.
    pub not_saved: &'static str,
    /// Starts a policy refusal's problems.
    pub not_sent: &'static str,
    /// No firewall service.
    pub no_daemon: &'static str,
    /// The bridge's own timeout, and no result in time.
    pub unknown: &'static str,
    /// OkWithNote: the note after `saved` (true), or the note alone.
    pub note_after_saved: bool,
}

pub const EDITOR_WORDING: Wording = Wording {
    saved: SAVED,
    not_saved: NOT_SAVED,
    not_sent: NOT_SENT,
    no_daemon: NO_DAEMON,
    unknown: UNKNOWN,
    note_after_saved: false,
};

/// What `outcome` says in `wording`.
pub fn finished_with(outcome: &RuleCommandOutcome, wording: &Wording) -> Finished {
    let (saved, status) = match outcome {
        RuleCommandOutcome::Ok => (true, wording.saved.to_string()),
        RuleCommandOutcome::OkWithNote { note } if wording.note_after_saved => {
            (true, format!("{} {note}", wording.saved))
        }
        RuleCommandOutcome::OkWithNote { note } => (true, note.clone()),
        RuleCommandOutcome::Rejected { reason } => {
            (false, format!("{}{reason}", wording.not_saved))
        }
        RuleCommandOutcome::Refused { problems } => (
            false,
            format!("{}{}", wording.not_sent, plain_problems(problems).join(" ")),
        ),
        RuleCommandOutcome::Timeout => (false, wording.unknown.to_string()),
        RuleCommandOutcome::NoDaemon => (false, wording.no_daemon.to_string()),
        RuleCommandOutcome::Unsure { reason } => (false, reason.clone()),
    };
    Finished { saved, status }
}

/// The editor's [`finished_with`].
pub fn finished(outcome: &RuleCommandOutcome) -> Finished {
    finished_with(outcome, &EDITOR_WORDING)
}

/// How long to wait for a result before giving up. A rename waits for up to
/// three daemon answers (5 s each) plus the reply, so well above that.
pub const NO_ANSWER_AFTER: Duration = Duration::from_secs(30);

/// The one request a sheet is waiting for, if any, with what the sheet
/// keeps about it (`Tag`: nothing for the editor; the row for "Make a
/// rule…").
#[derive(Debug)]
pub struct Pending<Tag = ()> {
    waiting: Option<(String, Tag, Instant)>,
}

impl<Tag> Default for Pending<Tag> {
    fn default() -> Self {
        Self { waiting: None }
    }
}

impl<Tag> Pending<Tag> {
    /// `request_id`, about `tag`, was sent at `now`.
    pub fn sent_with(&mut self, request_id: String, tag: Tag, now: Instant) {
        self.waiting = Some((request_id, tag, now));
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// The send failed: nothing to wait for.
    pub fn abandon(&mut self) {
        self.waiting = None;
    }

    /// The tag and what the result says in `wording`, when `message` is the
    /// awaited result; the wait ends there.
    pub fn result_with(
        &mut self,
        message: &ServerMessage,
        wording: &Wording,
    ) -> Option<(Tag, Finished)> {
        let ServerMessage::RuleCommandResult {
            request_id,
            outcome,
        } = message
        else {
            return None;
        };
        let (awaited, _, _) = self.waiting.as_ref()?;
        if awaited != request_id {
            return None;
        }
        let (_, tag, _) = self.waiting.take()?;
        Some((tag, finished_with(outcome, wording)))
    }

    /// Give up after `after` of silence, or at once when `gone(tag)`: the
    /// change may or may not have been made (`wording.unknown`).
    pub fn expired_with(
        &mut self,
        now: Instant,
        after: Duration,
        gone: impl Fn(&Tag) -> bool,
        wording: &Wording,
    ) -> Option<(Tag, Finished)> {
        let (_, tag, sent_at) = self.waiting.as_ref()?;
        if !gone(tag) && now.duration_since(*sent_at) <= after {
            return None;
        }
        let (_, tag, _) = self.waiting.take()?;
        Some((
            tag,
            Finished {
                saved: false,
                status: wording.unknown.to_string(),
            },
        ))
    }
}

impl Pending {
    /// `request_id` was sent at `now`.
    pub fn sent(&mut self, request_id: String, now: Instant) {
        self.sent_with(request_id, (), now);
    }

    /// The finished command, when `message` is the awaited result; the wait
    /// ends there.
    pub fn on_message(&mut self, message: &ServerMessage) -> Option<Finished> {
        self.result_with(message, &EDITOR_WORDING)
            .map(|(_, done)| done)
    }

    /// Give up after [`NO_ANSWER_AFTER`] of silence: the change may or may
    /// not have been made.
    pub fn poll(&mut self, now: Instant) -> Option<Finished> {
        self.expired_with(now, NO_ANSWER_AFTER, |_| false, &EDITOR_WORDING)
            .map(|(_, done)| done)
    }
}
