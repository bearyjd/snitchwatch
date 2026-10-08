//! "Make a rule…" for a row whose prompt was put off (prompt-slot plan Part
//! C, item 9): the rule the decision sheet's choices describe, sent as
//! `AddRule` (the daemon's `CHANGE_RULE`), until the rule editor exists.
//!
//! The rule comes from the bridge crate's own `verdict_to_rule`, so it is
//! bound to the program and refused for an unidentifiable one (#44) exactly
//! like the rule an answered prompt would have made. Only a remembered
//! duration makes a rule; a once-only answer can't be added afterwards.
//!
//! Two fixes on what the row carries (PR #98 security review):
//! - **No hostname.** The bridge shows the IP in `dst_host` when the daemon
//!   sent no host (`translator::connection::connection_to_row`), and a
//!   `dest.host == <ip>` rule never matches: the daemon's `DstHost` is
//!   empty. Such a row is built with no host, so the rule matches `dest.ip`.
//!   On port 53 `DstHost` can be a DNS question that equals the IP
//!   (`rules/simulator/prefill.rs`); `dest.ip` still matches there.
//! - **A name of its own.** The daemon's `CHANGE_RULE` replaces a rule of
//!   the same name, and `rule_name_for` gives the same name to the prompt's
//!   own rule, e.g. "Decide later"'s 5-minute block, whose expiry timer
//!   would then delete the new rule. A made rule's name ends in
//!   `-made-<unix ms>`.

use snitchwatch_bridge::cache::connections::Verdict;
use snitchwatch_bridge::rule_wire::rule_to_wire;
use snitchwatch_bridge::translator::verdict::verdict_to_rule;
use snitchwatch_bridge::ws_messages::{ClientMessage, ConnectionRow};
use snitchwatch_proto::protocol::Connection;

use crate::pending_decision::{parse_duration, parse_scope, VerdictChoice};

/// The `AddRule` for deferred `row` and the sheet's `choice`, `scope` and
/// `duration` tokens, made at `now_ms` (Unix ms), or `None` when no rule
/// may be made: the row wasn't put off, the duration is once-only, the
/// choice is unknown, or the bridge can't name the program.
pub(crate) fn add_rule_message(
    row: &ConnectionRow,
    choice: &str,
    scope: &str,
    duration: &str,
    now_ms: i64,
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
    let ip_stands_in = !row.dst_ip.is_empty() && row.dst_host == row.dst_ip;
    let conn = Connection {
        protocol: row.protocol.clone(),
        dst_host: if ip_stands_in {
            String::new()
        } else {
            row.dst_host.clone()
        },
        dst_ip: row.dst_ip.clone(),
        dst_port: u32::from(row.dst_port),
        process_path: row.process_path.clone().unwrap_or_default(),
        ..Default::default()
    };
    let made = verdict_to_rule(verdict, duration, parse_scope(scope), &conn, now_ms / 1000).ok()?;
    let rule = snitchwatch_proto::protocol::Rule {
        name: format!("{}-made-{now_ms}", made.name),
        ..made
    };
    Some(ClientMessage::AddRule {
        rule: rule_to_wire(&rule),
        request_id: None,
        reply: None,
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
            decided_by_default: false,
        }
    }

    fn wire_rule(msg: ClientMessage) -> serde_json::Value {
        match msg {
            ClientMessage::AddRule { rule, .. } => rule,
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
        let ClientMessage::AddRule { rule, .. } = msg else {
            unreachable!()
        };
        let notification = notification_for_effect(&UpstreamEffect::AddRule { rule }, 1)
            .expect("the bridge accepts the rule")
            .expect("as a CHANGE_RULE");
        assert_eq!(notification.rules.len(), 1);
    }

    fn operator_text(msg: ClientMessage) -> String {
        wire_rule(msg)["operator"].to_string()
    }

    #[test]
    fn a_row_with_no_hostname_makes_a_dest_ip_rule() {
        let named = deferred_row(Some("/usr/bin/curl"));
        let text =
            operator_text(add_rule_message(&named, "deny", "this_host", "forever", 0).unwrap());
        assert!(
            text.contains("dest.host") && !text.contains("dest.ip"),
            "{text}"
        );

        // The bridge put the IP in `dst_host`: the daemon's host was empty.
        let ip_only = ConnectionRow {
            dst_host: "93.184.216.34".into(),
            ..named.clone()
        };
        // On port 53 it may also be a DNS question equal to the IP.
        let dns_question = ConnectionRow {
            dst_port: 53,
            protocol: "udp".into(),
            ..ip_only.clone()
        };
        for row in [ip_only, dns_question] {
            for scope in ["this_host", "any_host_on_domain"] {
                let msg = add_rule_message(&row, "deny", scope, "forever", 0).unwrap();
                let text = operator_text(msg.clone());
                assert!(
                    text.contains("\"dest.ip\"") && text.contains("93.184.216.34"),
                    "{scope}: {text}"
                );
                assert!(!text.contains("dest.host"), "{scope}: {text}");
                let ClientMessage::AddRule { rule, .. } = msg else {
                    unreachable!()
                };
                notification_for_effect(&UpstreamEffect::AddRule { rule }, 1).unwrap();
            }
        }
    }

    #[test]
    fn a_made_rule_never_takes_the_name_of_the_prompts_own_rule() {
        use snitchwatch_bridge::rule_name::{is_reserved_name, validate_rule_name};
        use snitchwatch_bridge::translator::verdict::rule_name_for;
        let row = deferred_row(Some("/usr/bin/curl"));
        let name = |now_ms| {
            wire_rule(add_rule_message(&row, "deny", "this_host", "forever", now_ms).unwrap())
                ["name"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let prompts_own = rule_name_for(Verdict::Deny, "example.com", 443, "/usr/bin/curl");
        let made = name(1_700_000_000_123);
        assert_eq!(made, format!("{prompts_own}-made-1700000000123"));
        assert_ne!(name(1_700_000_000_124), made);
        validate_rule_name(&made).unwrap();
        assert!(!is_reserved_name(&made));
    }

    #[test]
    fn every_remembered_duration_and_scope_makes_a_rule() {
        let row = deferred_row(Some("/usr/bin/curl"));
        for duration in ["for_5_minutes", "until_quit", "forever"] {
            for scope in ["this_host", "any_host_on_domain", "any_host"] {
                for choice in ["allow", "deny"] {
                    let msg = add_rule_message(&row, choice, scope, duration, 0)
                        .unwrap_or_else(|| panic!("{choice} {scope} {duration}"));
                    let ClientMessage::AddRule { rule, .. } = msg else {
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
