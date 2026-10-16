//! The results of the Rules page's own rule commands (#152): its
//! inspector's switch and its Delete buttons send a request id, and the
//! bridge's `RuleCommandResult` for it leaves a note on its rule, without
//! Qt. `crate::rules_model` binds it to QML.
//!
//! Notes are kept per rule (PR #163 review): a command or result for one
//! rule never clears another's note. A rule's note goes when a new command
//! for that rule is sent, when its result is an Ok, and when a rule list
//! shows the rule changed after the command (the note may contradict it).
//!
//! What "changed" means doesn't depend on whether the result or the list
//! the command changed comes first (the bridge's outbound `select!` isn't
//! ordered, and a running import holds lists back): each note expects the
//! state the bridge's cache holds after its outcome. An `OkWithNote` from
//! this page is a change the cache applied (`apply_refused`: a refused
//! turn-off of an `always` rule, a recommended one too, is applied, not
//! saved; a refused delete is removed): the rule as sent, or no rule. Any
//! other outcome left the cache as it was (an unknown change is only
//! noted as maybe applied, off the list): the rule as it was when sent.
//! An applied toggle expects only the rule's switch: the bridge lists the
//! rule it rebuilt from what was sent, which needn't serialize exactly as
//! the GUI's copy did. Until a list shows the expected state, one still
//! showing the rule as before the command is an older list and keeps the
//! note; any other state drops it. Once a list showed it, the rule as that
//! list showed it is the note's, and any change from it drops the note.
//!
//! A new bridge session drops every note and every wait: a result goes
//! only to the connection that asked, and the resynced list shows what the
//! daemon holds.
//! Every note is plain text.

use std::collections::VecDeque;

use serde::Serialize;
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};

use super::editor_view::{finished_with, Wording};

/// Most requests waited for at once; the oldest is forgotten past this.
pub const MAX_WAITING: usize = 32;
/// Most notes kept; the oldest goes past this.
pub const MAX_NOTES: usize = 16;

/// What a Rules page result says: an Ok says nothing (the list shows it),
/// an `OkWithNote` its note, an `Unsure` its reason.
pub const RULES_PAGE_WORDING: Wording = Wording {
    saved: "",
    not_saved: "Not changed: ",
    not_sent: "Not sent: ",
    no_daemon: "The firewall service isn't connected, so nothing was changed.",
    unknown: "No answer from the firewall in time. The change may or may not have been made.",
    note_after_saved: false,
};

/// A rule as the list shows it now (`None`: not listed), to tell whether
/// it changed since a note came.
pub type RuleState = Option<String>;

/// What a command does to its rule once applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// A toggle: the rule, switched on (`true`) or off.
    Enabled(bool),
    /// A delete: no rule.
    Absent,
}

/// What a note expects the list to show.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expected {
    Exactly(RuleState),
    Applied(Applied),
}

impl Expected {
    fn shown_by(&self, now: &RuleState) -> bool {
        match self {
            Self::Exactly(state) => state == now,
            Self::Applied(Applied::Absent) => now.is_none(),
            Self::Applied(Applied::Enabled(on)) => now.as_deref().is_some_and(|json| {
                serde_json::from_str::<serde_json::Value>(json)
                    .is_ok_and(|rule| rule["enabled"].as_bool() == Some(*on))
            }),
        }
    }
}

/// A request waiting for its result.
#[derive(Debug)]
struct Waiting {
    request_id: String,
    rule: String,
    shown: String,
    /// The rule when the command was sent.
    before: RuleState,
    applied: Applied,
}

/// A rule's note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    /// The rule's name (its identity).
    pub rule: String,
    /// Its name as shown (`row_store::Rule::shown_name`).
    pub shown: String,
    pub text: String,
    /// The rule as the cache holds it after the outcome.
    #[serde(skip)]
    expected: Expected,
    #[serde(skip)]
    before: RuleState,
    /// The rule as the first list to show `expected` showed it: from then
    /// on any other state is a change.
    #[serde(skip)]
    settled: Option<RuleState>,
}

impl Note {
    /// Whether the note still holds against the rule shown as `now`.
    fn holds(&mut self, now: &RuleState) -> bool {
        if let Some(settled) = &self.settled {
            return settled == now;
        }
        if self.expected.shown_by(now) {
            self.settled = Some(now.clone());
            return true;
        }
        *now == self.before
    }
}

/// The requests the Rules page is waiting for, and its notes.
#[derive(Debug, Default)]
pub struct CommandNotes {
    waiting: VecDeque<Waiting>,
    notes: Vec<Note>,
    session: Option<u64>,
}

impl CommandNotes {
    /// `request_id`, about rule `rule` (shown as `shown`), was sent: the
    /// rule was `before`, and is `applied` once the command applies. That
    /// rule's note goes. Returns whether the notes changed.
    pub fn sent(
        &mut self,
        request_id: String,
        rule: &str,
        shown: &str,
        before: RuleState,
        applied: Applied,
    ) -> bool {
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back(Waiting {
            request_id,
            rule: rule.to_string(),
            shown: shown.to_string(),
            before,
            applied,
        });
        self.forget(rule)
    }

    /// `Some(changed)` when `message` is the result of a request this page
    /// is waiting for (the wait ends there), with `state` the list's rules
    /// now; `None` otherwise.
    pub fn on_message(
        &mut self,
        message: &ServerMessage,
        state: impl Fn(&str) -> RuleState,
    ) -> Option<bool> {
        let ServerMessage::RuleCommandResult {
            request_id,
            outcome,
        } = message
        else {
            return None;
        };
        let at = self
            .waiting
            .iter()
            .position(|waiting| &waiting.request_id == request_id)?;
        let waiting = self.waiting.remove(at)?;
        let text = finished_with(outcome, &RULES_PAGE_WORDING).status;
        let forgot = self.forget(&waiting.rule);
        let expected = match outcome {
            RuleCommandOutcome::OkWithNote { .. } => Expected::Applied(waiting.applied),
            _ => Expected::Exactly(waiting.before.clone()),
        };
        let mut note = Note {
            rule: waiting.rule,
            shown: waiting.shown,
            text,
            expected,
            before: waiting.before,
            settled: None,
        };
        if note.text.is_empty() || !note.holds(&state(&note.rule)) {
            return Some(forgot);
        }
        if self.notes.len() >= MAX_NOTES {
            self.notes.remove(0);
        }
        self.notes.push(note);
        Some(true)
    }

    /// The rule list changed (`state`: its rules now): notes on rules that
    /// changed after their command go. Returns whether any did.
    pub fn rules_changed(&mut self, state: impl Fn(&str) -> RuleState) -> bool {
        let before = self.notes.len();
        self.notes.retain_mut(|note| note.holds(&state(&note.rule)));
        self.notes.len() != before
    }

    /// A message from bridge session `connection_id`: a new session drops
    /// every wait and note. Returns whether the notes changed.
    pub fn note_session(&mut self, connection_id: u64) -> bool {
        let new = self.session.is_some_and(|id| id != connection_id);
        self.session = Some(connection_id);
        if !new {
            return false;
        }
        self.waiting.clear();
        let had = !self.notes.is_empty();
        self.notes.clear();
        had
    }

    /// The notes, oldest first.
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// The notes as one plain-text line each, "<rule>: <note>".
    pub fn text(&self) -> String {
        self.notes
            .iter()
            .map(|note| format!("{}: {}", note.shown, note.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn forget(&mut self, rule: &str) -> bool {
        let before = self.notes.len();
        self.notes.retain(|note| note.rule != rule);
        self.notes.len() != before
    }
}

#[cfg(test)]
#[path = "command_notes_tests.rs"]
mod tests;
