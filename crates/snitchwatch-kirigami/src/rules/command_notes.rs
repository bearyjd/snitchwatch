//! The results of the Rules page's own rule commands (#152): its
//! inspector's switch and its Delete buttons send a request id, and the
//! bridge's `RuleCommandResult` for it leaves a note for the page, without
//! Qt. `crate::rules_model` binds it to QML.
//!
//! Only results this page is waiting for count. Results reach it only from
//! the live bridge session (`crate::result_feed`), and request ids are
//! unique in the process, so a result for a request sent to an earlier
//! session can't arrive; its id just ages out of the bounded wait list.
//! Every note is plain text.

use std::collections::VecDeque;

use snitchwatch_bridge::ws_messages::ServerMessage;

use super::editor_view::{finished_with, Wording};

/// Most requests waited for at once; the oldest is forgotten past this.
pub const MAX_WAITING: usize = 32;

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

/// A note and the rule it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub rule: String,
    pub text: String,
}

/// The requests the Rules page is waiting for: their ids and rule names.
#[derive(Debug, Default)]
pub struct CommandNotes {
    waiting: VecDeque<(String, String)>,
}

impl CommandNotes {
    /// `request_id`, about rule `rule`, was sent.
    pub fn sent(&mut self, request_id: String, rule: String) {
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.pop_front();
        }
        self.waiting.push_back((request_id, rule));
    }

    /// The note `message` leaves when it is the result of a request this
    /// page is waiting for ("" for an Ok); the wait for it ends there.
    pub fn on_message(&mut self, message: &ServerMessage) -> Option<Note> {
        let ServerMessage::RuleCommandResult {
            request_id,
            outcome,
        } = message
        else {
            return None;
        };
        let at = self.waiting.iter().position(|(id, _)| id == request_id)?;
        let (_, rule) = self.waiting.remove(at)?;
        Some(Note {
            rule,
            text: finished_with(outcome, &RULES_PAGE_WORDING).status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::rule_policy::RuleProblem;
    use snitchwatch_bridge::ws_messages::RuleCommandOutcome;

    fn result(id: &str, outcome: RuleCommandOutcome) -> ServerMessage {
        ServerMessage::RuleCommandResult {
            request_id: id.into(),
            outcome,
        }
    }

    #[test]
    fn each_awaited_result_leaves_its_note_once() {
        let mut notes = CommandNotes::default();
        notes.sent("rules-1".into(), "100-x".into());
        let note = RuleCommandOutcome::OkWithNote {
            note: "may come back on".into(),
        };
        assert_eq!(
            notes.on_message(&result("rules-1", note.clone())),
            Some(Note {
                rule: "100-x".into(),
                text: "may come back on".into()
            })
        );
        assert_eq!(notes.on_message(&result("rules-1", note)), None, "twice");
        notes.sent("rules-2".into(), "100-x".into());
        assert_eq!(
            notes
                .on_message(&result("rules-2", RuleCommandOutcome::Ok))
                .map(|note| note.text),
            Some(String::new())
        );
    }

    #[test]
    fn a_result_nobody_here_asked_for_is_ignored() {
        let mut notes = CommandNotes::default();
        notes.sent("rules-1".into(), "100-x".into());
        let editor = result("edit-1", RuleCommandOutcome::Ok);
        assert_eq!(notes.on_message(&editor), None);
        assert_eq!(
            notes.on_message(&ServerMessage::SetRules { rules: vec![] }),
            None
        );
    }

    #[test]
    fn every_outcome_says_what_happened_in_plain_words() {
        let cases = [
            (
                RuleCommandOutcome::Rejected {
                    reason: "bad".into(),
                },
                "Not changed: bad",
            ),
            (
                RuleCommandOutcome::Refused {
                    problems: vec![RuleProblem {
                        path: "name".into(),
                        reason: "busy".into(),
                    }],
                },
                "Not sent: ",
            ),
            (
                RuleCommandOutcome::Unsure {
                    reason: "may or may not have applied".into(),
                },
                "may or may not have applied",
            ),
            (RuleCommandOutcome::Timeout, RULES_PAGE_WORDING.unknown),
            (RuleCommandOutcome::NoDaemon, RULES_PAGE_WORDING.no_daemon),
        ];
        for (outcome, starts) in cases {
            let mut notes = CommandNotes::default();
            notes.sent("r".into(), "100-x".into());
            let note = notes
                .on_message(&result("r", outcome.clone()))
                .unwrap()
                .text;
            assert!(note.starts_with(starts), "{outcome:?}: {note}");
            assert!(!note.is_empty(), "{outcome:?}");
        }
    }

    #[test]
    fn the_wait_list_is_bounded() {
        let mut notes = CommandNotes::default();
        for n in 0..=MAX_WAITING {
            notes.sent(format!("r{n}"), "100-x".into());
        }
        assert_eq!(
            notes.on_message(&result("r0", RuleCommandOutcome::Ok)),
            None
        );
        let last = format!("r{MAX_WAITING}");
        assert!(notes
            .on_message(&result(&last, RuleCommandOutcome::Ok))
            .is_some());
    }
}
