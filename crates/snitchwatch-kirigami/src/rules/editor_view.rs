//! The rule editor's messages and texts (roadmap P2.1), without Qt:
//! what is sent, when saving is allowed, what each result says, and
//! whether a cached rule can be edited at all. `crate::rule_editor_controller`
//! binds it to QML. Every string is plain text.

use serde::Serialize;
use serde_json::Value;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome, ServerMessage};

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

pub fn finished(outcome: &RuleCommandOutcome) -> Finished {
    let (saved, status) = match outcome {
        RuleCommandOutcome::Ok => (true, SAVED.to_string()),
        RuleCommandOutcome::Rejected { reason } => (false, format!("{NOT_SAVED}{reason}")),
        RuleCommandOutcome::Refused { problems } => (
            false,
            format!("{NOT_SENT}{}", plain_problems(problems).join(" ")),
        ),
        RuleCommandOutcome::Timeout => (false, UNKNOWN.to_string()),
        RuleCommandOutcome::NoDaemon => (false, NO_DAEMON.to_string()),
        RuleCommandOutcome::Unsure { reason } => (false, reason.clone()),
    };
    Finished { saved, status }
}

/// Whether `message` is the result the editor is waiting for.
pub fn awaits(waiting: Option<&str>, message: &ServerMessage) -> bool {
    matches!(
        (waiting, message),
        (Some(id), ServerMessage::RuleCommandResult { request_id, .. }) if request_id == id
    )
}

/// The live feed's filter for the editor.
pub fn interests_rule_editor(message: &ServerMessage) -> bool {
    matches!(message, ServerMessage::RuleCommandResult { .. })
}
