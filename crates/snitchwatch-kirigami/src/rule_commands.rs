//! What the rule-command sheets (the rule editor, "Make a rule…") share about
//! talking to the bridge, without Qt (PR #111 review, L6): the texts for a
//! message that never reached it, request ids, the results-only feed filter,
//! and a local row id's bridge session.

use std::sync::atomic::{AtomicU64, Ordering};

use snitchwatch_bridge::ws_messages::ServerMessage;

use crate::bridge_runtime::SendClientMessageError;

/// A message that never reached the bridge ([`not_sent_text`]).
pub const NOT_CONNECTED: &str = "Snitchwatch isn't connected to its service, so nothing was sent.";
pub const QUEUE_FULL: &str = "Snitchwatch is busy, so nothing was sent. Try again in a moment.";

/// Why a message wasn't queued for the bridge, as plain text.
pub fn not_sent_text(error: SendClientMessageError) -> &'static str {
    match error {
        SendClientMessageError::Full => QUEUE_FULL,
        SendClientMessageError::Disconnected
        | SendClientMessageError::Stopped
        | SendClientMessageError::StaleSession => NOT_CONNECTED,
    }
}

/// A new request id, `<prefix>-<pid>-<n>`, unique in this process (the
/// editor's are `edit-…`, "Make a rule…"'s `make-…`). Valid for the bridge
/// for a short ASCII `prefix`.
pub fn next_request_id(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The rule-command sheets' feed filter: results only.
pub fn interests_rule_results(message: &ServerMessage) -> bool {
    matches!(message, ServerMessage::RuleCommandResult { .. })
}

/// Local-only row identity: the bridge session and the bridge's own row id.
/// Never transmitted to the service.
pub fn split_session_row_id(id: &str) -> Option<(u64, &str)> {
    let (session, wire_id) = id.split_once(':')?;
    let session = session.parse::<u64>().ok().filter(|id| *id != 0)?;
    (!wire_id.is_empty()).then_some((session, wire_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::RuleCommandOutcome;

    /// The texts for a message that never reached the bridge, shared by the
    /// editor and "Make a rule…".
    #[test]
    fn a_message_that_was_not_queued_says_why() {
        use SendClientMessageError as E;
        assert_eq!(not_sent_text(E::Full), QUEUE_FULL);
        for error in [E::Disconnected, E::Stopped, E::StaleSession] {
            assert_eq!(not_sent_text(error), NOT_CONNECTED, "{error:?}");
        }
    }

    #[test]
    fn request_ids_are_valid_distinct_and_prefixed() {
        let (edit, make) = (next_request_id("edit"), next_request_id("make"));
        assert!(
            edit.starts_with("edit-") && make.starts_with("make-"),
            "{edit} {make}"
        );
        assert_ne!(edit, next_request_id("edit"));
        for id in [&edit, &make] {
            assert!(
                snitchwatch_bridge::ws_messages::valid_request_id(id),
                "{id}"
            );
        }
    }

    #[test]
    fn the_feed_takes_only_results() {
        assert!(interests_rule_results(&ServerMessage::RuleCommandResult {
            request_id: "x".into(),
            outcome: RuleCommandOutcome::Ok,
        }));
        assert!(!interests_rule_results(&ServerMessage::SetRules {
            rules: vec![]
        }));
    }

    #[test]
    fn local_row_identity_retains_origin_even_when_wire_ids_are_reused() {
        assert_eq!(split_session_row_id("1:7"), Some((1, "7")));
        assert_eq!(split_session_row_id("2:7"), Some((2, "7")));
        assert_eq!(split_session_row_id("1:event-123"), Some((1, "event-123")));
        for id in ["7", "0:7", "invalid:7", "2:"] {
            assert_eq!(split_session_row_id(id), None);
        }
    }
}
