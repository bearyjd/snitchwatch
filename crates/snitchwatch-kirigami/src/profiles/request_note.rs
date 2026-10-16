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

/// The note `message` sets, if it answers the request `awaiting`: the
/// refusal's reasons for a plain-text label, or "" for an ok. `None` for
/// any other message, including another request's result.
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
    Some(match outcome {
        RuleCommandOutcome::Refused { problems } => {
            let reasons: Vec<String> = problems
                .iter()
                .map(|p| strip_display_hazards(&p.reason))
                .collect();
            if reasons.is_empty() {
                "The change was refused.".to_string()
            } else {
                reasons.join("; ")
            }
        }
        RuleCommandOutcome::Ok => String::new(),
        _ => "The change wasn't saved.".to_string(),
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
}
