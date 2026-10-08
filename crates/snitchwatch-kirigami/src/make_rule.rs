//! "Make a rule…" for a row whose prompt was put off (prompt-slot plan Part
//! C, item 9): the rule the decision sheet's choices describe, sent as
//! `AddRule` (the daemon's `CHANGE_RULE`), until the rule editor exists.
//!
//! The rule comes from the bridge crate's own `verdict_to_rule`, so it is
//! bound to the program and refused for an unidentifiable one (#44) exactly
//! like the rule an answered prompt would have made. Only a remembered
//! duration makes a rule; a once-only answer can't be added afterwards.

use snitchwatch_bridge::cache::connections::Verdict;
use snitchwatch_bridge::rule_wire::rule_to_wire;
use snitchwatch_bridge::translator::verdict::verdict_to_rule;
use snitchwatch_bridge::ws_messages::{ClientMessage, ConnectionRow};
use snitchwatch_proto::protocol::Connection;

use crate::pending_decision::{parse_duration, parse_scope, VerdictChoice};

/// The `AddRule` for deferred `row` and the sheet's `choice`, `scope` and
/// `duration` tokens, or `None` when no rule may be made: the row wasn't put
/// off, the duration is once-only, the choice is unknown, or the bridge
/// can't name the program.
pub(crate) fn add_rule_message(
    row: &ConnectionRow,
    choice: &str,
    scope: &str,
    duration: &str,
    now_secs: i64,
) -> Option<ClientMessage> {
    if !row.deferred {
        return None;
    }
    let verdict = match VerdictChoice::from_token(choice)? {
        VerdictChoice::Allow => Verdict::Allow,
        VerdictChoice::Deny => Verdict::Deny,
    };
    let duration = parse_duration(duration);
    if !duration.remembers() {
        return None;
    }
    let conn = Connection {
        protocol: row.protocol.clone(),
        dst_host: row.dst_host.clone(),
        dst_ip: row.dst_ip.clone(),
        dst_port: u32::from(row.dst_port),
        process_path: row.process_path.clone().unwrap_or_default(),
        ..Default::default()
    };
    let rule = verdict_to_rule(verdict, duration, parse_scope(scope), &conn, now_secs).ok()?;
    Some(ClientMessage::AddRule {
        rule: rule_to_wire(&rule),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
    use snitchwatch_bridge::translator::upstream::UpstreamEffect;

    fn deferred_row(process_path: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: "1:ask-1".into(),
            process: "curl".into(),
            process_path: process_path.map(str::to_owned),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: true,
        }
    }

    fn wire_rule(msg: ClientMessage) -> serde_json::Value {
        match msg {
            ClientMessage::AddRule { rule } => rule,
            other => panic!("expected AddRule, got {other:?}"),
        }
    }

    #[test]
    fn a_rule_for_a_put_off_row_is_bound_to_the_program_and_the_bridge_accepts_it() {
        let row = deferred_row(Some("/usr/bin/curl"));
        let msg = add_rule_message(&row, "deny", "this_host", "forever", 1_700_000_000).unwrap();
        let rule = wire_rule(msg.clone());
        assert_eq!(rule["action"], "deny");
        assert_eq!(rule["duration"], "always");
        let text = rule["operator"].to_string();
        assert!(
            text.contains("process.path") && text.contains("/usr/bin/curl"),
            "{text}"
        );
        assert!(text.contains("example.com"), "{text}");
        // What the bridge's pump does with a GUI's AddRule.
        let ClientMessage::AddRule { rule } = msg else {
            unreachable!()
        };
        let notification = notification_for_effect(&UpstreamEffect::AddRule { rule }, 1)
            .expect("the bridge accepts the rule")
            .expect("as a CHANGE_RULE");
        assert_eq!(notification.rules.len(), 1);
    }

    #[test]
    fn every_remembered_duration_and_scope_makes_a_rule() {
        let row = deferred_row(Some("/usr/bin/curl"));
        for duration in ["for_5_minutes", "until_quit", "forever"] {
            for scope in ["this_host", "any_host_on_domain", "any_host"] {
                for choice in ["allow", "deny"] {
                    let msg = add_rule_message(&row, choice, scope, duration, 0)
                        .unwrap_or_else(|| panic!("{choice} {scope} {duration}"));
                    let ClientMessage::AddRule { rule } = msg else {
                        unreachable!()
                    };
                    notification_for_effect(&UpstreamEffect::AddRule { rule }, 1).unwrap();
                }
            }
        }
    }

    #[test]
    fn no_rule_once_only_for_an_unknown_program_or_a_row_that_was_not_put_off() {
        let row = deferred_row(Some("/usr/bin/curl"));
        assert!(add_rule_message(&row, "deny", "this_host", "this_time", 0).is_none());
        assert!(add_rule_message(&row, "maybe", "this_host", "forever", 0).is_none());
        for path in [None, Some("Kernel connection"), Some("curl")] {
            assert!(
                add_rule_message(&deferred_row(path), "deny", "this_host", "forever", 0).is_none(),
                "{path:?}"
            );
        }
        let answered = ConnectionRow {
            deferred: false,
            ..row
        };
        assert!(add_rule_message(&answered, "deny", "this_host", "forever", 0).is_none());
    }
}
