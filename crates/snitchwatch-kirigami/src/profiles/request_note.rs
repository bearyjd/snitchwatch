//! The Profiles page's note for a refused create/rename/matcher change
//! (issue #116): the bridge answers a request that carried an id with a
//! `RuleCommandResult`, and a refusal's reasons are shown under the form.

use std::sync::atomic::{AtomicU64, Ordering};

use snitchwatch_bridge::translator::verdict::strip_display_hazards;
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};

/// A request id no other request of this process (or another GUI) uses.
pub fn next_request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "pf-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The request whose answer sets the note.
#[derive(Debug, Default)]
pub struct Awaiting(Option<String>);

impl Awaiting {
    /// Start waiting on a new request; returns its id.
    pub fn expect(&mut self) -> String {
        let id = next_request_id();
        self.0 = Some(id.clone());
        id
    }

    /// Route `message`. `None`: not a rule command result, so the caller
    /// handles it (and must not skip it). `Some(note)`: it was one and is
    /// consumed; `note` is the new text if it answered the awaited request
    /// (which then stops being awaited), else `None`.
    pub fn on_message(&mut self, message: &ServerMessage) -> Option<Option<String>> {
        if !matches!(message, ServerMessage::RuleCommandResult { .. }) {
            return None;
        }
        let note = note_for(self.0.as_deref(), message);
        if note.is_some() {
            self.0 = None;
        }
        Some(note)
    }
}

/// The note `message` sets, if it answers the request `awaiting`: what to
/// show for the outcome ("" when it succeeded). `None` for any other
/// message, including another request's result.
pub fn note_for(awaiting: Option<&str>, message: &ServerMessage) -> Option<String> {
    let ServerMessage::RuleCommandResult {
        request_id,
        outcome,
    } = message
    else {
        return None;
    };
    if awaiting != Some(request_id.as_str()) {
        return None;
    }
    let plain = |text: &str| strip_display_hazards(text);
    Some(match outcome {
        RuleCommandOutcome::Ok => String::new(),
        RuleCommandOutcome::OkWithNote { note } => plain(note),
        RuleCommandOutcome::Rejected { reason } => plain(reason),
        RuleCommandOutcome::Refused { problems } => {
            let reasons: Vec<String> = problems.iter().map(|p| plain(&p.reason)).collect();
            if reasons.is_empty() {
                "The change was refused.".to_string()
            } else {
                reasons.join("; ")
            }
        }
        RuleCommandOutcome::Unsure { reason } => {
            let reason = plain(reason);
            if reason.is_empty() {
                "Whether the change was saved is unknown.".to_string()
            } else {
                reason
            }
        }
        RuleCommandOutcome::Timeout | RuleCommandOutcome::NoDaemon => {
            "The change wasn't saved.".to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::rule_policy::RuleProblem;

    fn result(id: &str, outcome: RuleCommandOutcome) -> ServerMessage {
        ServerMessage::RuleCommandResult {
            request_id: id.into(),
            outcome,
        }
    }

    fn refused(reason: &str) -> RuleCommandOutcome {
        RuleCommandOutcome::Refused {
            problems: vec![RuleProblem {
                path: "id".into(),
                reason: reason.into(),
            }],
        }
    }

    #[test]
    fn a_refusal_of_the_awaited_request_becomes_the_note() {
        let note = note_for(Some("a"), &result("a", refused("already exists")));
        assert_eq!(note.as_deref(), Some("already exists"));
    }

    #[test]
    fn an_ok_clears_the_note() {
        assert_eq!(
            note_for(Some("a"), &result("a", RuleCommandOutcome::Ok)).as_deref(),
            Some("")
        );
    }

    #[test]
    fn another_requests_result_and_other_messages_are_ignored() {
        assert_eq!(note_for(Some("a"), &result("b", refused("x"))), None);
        assert_eq!(note_for(None, &result("a", refused("x"))), None);
        assert_eq!(
            note_for(Some("a"), &ServerMessage::ClearConnectionRows),
            None
        );
    }

    #[test]
    fn the_reason_is_plain_text_without_hidden_characters() {
        let note = note_for(Some("a"), &result("a", refused("ba\u{202e}d")));
        assert_eq!(note.as_deref(), Some("bad"));
    }

    #[test]
    fn request_ids_are_valid_and_distinct() {
        let (a, b) = (next_request_id(), next_request_id());
        assert_ne!(a, b);
        assert!(snitchwatch_bridge::ws_messages::valid_request_id(&a));
    }

    #[test]
    fn every_outcome_has_its_own_note() {
        let note = |outcome| note_for(Some("a"), &result("a", outcome)).unwrap();
        assert_eq!(note(RuleCommandOutcome::Ok), "");
        assert_eq!(
            note(RuleCommandOutcome::OkWithNote {
                note: "saved, but x".into()
            }),
            "saved, but x"
        );
        assert_eq!(
            note(RuleCommandOutcome::Rejected {
                reason: "no".into()
            }),
            "no"
        );
        assert_eq!(note(refused("taken")), "taken");
        assert_eq!(
            note(RuleCommandOutcome::Refused { problems: vec![] }),
            "The change was refused."
        );
        assert_eq!(
            note(RuleCommandOutcome::Unsure {
                reason: "maybe".into()
            }),
            "maybe"
        );
        assert_eq!(
            note(RuleCommandOutcome::Unsure { reason: "".into() }),
            "Whether the change was saved is unknown."
        );
        assert_eq!(
            note(RuleCommandOutcome::Timeout),
            "The change wasn't saved."
        );
        assert_eq!(
            note(RuleCommandOutcome::NoDaemon),
            "The change wasn't saved."
        );
    }

    #[test]
    fn a_result_is_always_consumed_but_only_the_awaited_one_sets_the_note() {
        let mut awaiting = Awaiting::default();
        let id = awaiting.expect();
        // Not a result: left to the caller.
        assert_eq!(
            awaiting.on_message(&ServerMessage::ClearConnectionRows),
            None
        );
        // An unrelated result: consumed, no note, still waiting.
        assert_eq!(
            awaiting.on_message(&result("other", refused("x"))),
            Some(None)
        );
        // The awaited one: consumed, sets the note, stops waiting.
        assert_eq!(
            awaiting.on_message(&result(&id, refused("taken"))),
            Some(Some("taken".to_string()))
        );
        assert_eq!(
            awaiting.on_message(&result(&id, refused("again"))),
            Some(None)
        );
    }
}
