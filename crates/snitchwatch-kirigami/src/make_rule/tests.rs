use super::*;
use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
use snitchwatch_bridge::translator::upstream::UpstreamEffect;
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};
use std::time::{Duration, Instant};

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
    let msg = add_rule_message(
        &row,
        "deny",
        "this_host",
        "forever",
        1_700_000_000,
        "make-1",
    )
    .unwrap();
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
    let text = operator_text(
        add_rule_message(&named, "deny", "this_host", "forever", 0, "make-1").unwrap(),
    );
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
            let msg = add_rule_message(&row, "deny", scope, "forever", 0, "make-1").unwrap();
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
        wire_rule(add_rule_message(&row, "deny", "this_host", "forever", now_ms, "make-1").unwrap())
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
                let msg = add_rule_message(&row, choice, scope, duration, 0, "make-1")
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
    assert!(add_rule_message(&row, "deny", "this_host", "this_time", 0, "make-1").is_none());
    assert!(add_rule_message(&row, "maybe", "this_host", "forever", 0, "make-1").is_none());
    for path in [None, Some("Kernel connection"), Some("curl")] {
        assert!(
            add_rule_message(
                &deferred_row(path),
                "deny",
                "this_host",
                "forever",
                0,
                "make-1"
            )
            .is_none(),
            "{path:?}"
        );
    }
    let answered = ConnectionRow {
        deferred: false,
        ..row
    };
    assert!(add_rule_message(&answered, "deny", "this_host", "forever", 0, "make-1").is_none());
    // A rule decided it: nothing to make here.
    let rule_matched = ConnectionRow {
        action: Some("allow".into()),
        matched_rule: Some("899-curl-allow".into()),
        ..answered
    };
    assert!(!offers_make_rule(&rule_matched));
    assert!(add_rule_message(&rule_matched, "deny", "this_host", "forever", 0, "make-1").is_none());
}

/// E3: a connection the firewall's default action decided gets the same
/// "Make a rule…" as a put-off one, through the same checks.
#[test]
fn a_row_the_default_action_decided_can_get_a_rule_like_a_put_off_one() {
    let by_default = ConnectionRow {
        id: "1:event-7".into(),
        action: Some("deny".into()),
        deferred: false,
        decided_by_default: true,
        ..deferred_row(Some("/usr/bin/curl"))
    };
    assert!(offers_make_rule(&by_default));
    assert!(offers_make_rule(&deferred_row(None)));
    let rule = wire_rule(
        add_rule_message(
            &by_default,
            "allow",
            "this_host",
            "forever",
            1_700_000_000,
            "make-1",
        )
        .unwrap(),
    );
    assert_eq!(rule["action"], "allow");
    let text = rule["operator"].to_string();
    assert!(
        text.contains("process.path") && text.contains("/usr/bin/curl"),
        "{text}"
    );
    // The same refusals: once-only, an unknown program.
    assert!(add_rule_message(&by_default, "deny", "this_host", "this_time", 0, "make-1").is_none());
    let unknown = ConnectionRow {
        process_path: None,
        ..by_default
    };
    assert!(add_rule_message(&unknown, "deny", "this_host", "forever", 0, "make-1").is_none());
}

// -- M1 (PR #108 security review): the bridge's outcome, not the send --

fn result(request_id: &str, outcome: RuleCommandOutcome) -> ServerMessage {
    ServerMessage::RuleCommandResult {
        request_id: request_id.to_string(),
        outcome,
    }
}

#[test]
fn the_add_asks_for_a_result_and_a_request_id_is_required() {
    let row = deferred_row(Some("/usr/bin/curl"));
    let ClientMessage::AddRule { request_id, .. } =
        add_rule_message(&row, "deny", "this_host", "forever", 0, "make-7").unwrap()
    else {
        panic!("expected AddRule");
    };
    assert_eq!(request_id.as_deref(), Some("make-7"));
    for bad in ["", "no spaces", &"x".repeat(65)] {
        assert!(
            add_rule_message(&row, "deny", "this_host", "forever", 0, bad).is_none(),
            "{bad:?}"
        );
    }
}

#[test]
fn only_an_ok_result_says_the_rule_was_created() {
    let now = Instant::now();
    let mut wait = MakeRuleWait::default();
    wait.begin("make-1".into(), "1:ask-1".into(), now);
    assert!(wait.is_waiting());
    // Another request's result, or another message, isn't ours.
    assert!(wait
        .on_message(&result("make-2", RuleCommandOutcome::Ok))
        .is_none());
    assert!(wait
        .on_message(&ServerMessage::ClearConnectionRows)
        .is_none());
    let (row, done, ending) = wait
        .on_message(&result("make-1", RuleCommandOutcome::Ok))
        .unwrap();
    assert_eq!(row, "1:ask-1");
    assert_eq!(ending, Ending::Created);
    assert_eq!(
        done,
        Finished {
            saved: true,
            status: CREATED.into()
        }
    );
    assert!(!wait.is_waiting());
}

#[test]
fn a_refusal_says_why_and_is_not_created() {
    let cases = [
        (
            RuleCommandOutcome::Rejected {
                reason: "a rule with this name exists".into(),
            },
            "The rule wasn't created: a rule with this name exists",
        ),
        (
            RuleCommandOutcome::NoDaemon,
            "The firewall service isn't connected, so the rule wasn't sent.",
        ),
        (
            RuleCommandOutcome::Unsure {
                reason: "unclear".into(),
            },
            "unclear",
        ),
        (RuleCommandOutcome::Timeout, NO_ANSWER),
    ];
    for (outcome, text) in cases {
        let mut wait = MakeRuleWait::default();
        wait.begin("make-1".into(), "1:event-5-1".into(), Instant::now());
        let (_, done, ending) = wait.on_message(&result("make-1", outcome)).unwrap();
        assert!(!done.saved, "{text}");
        assert_ne!(ending, Ending::Created, "{text}");
        assert_eq!(done.status, text);
    }
    let refused = outcome_status(&RuleCommandOutcome::Refused { problems: vec![] });
    assert!(!refused.saved);
    assert!(
        refused.status.starts_with("The rule wasn't sent: "),
        "{}",
        refused.status
    );
}

/// The bridge's own policy problems, in the editor's plain words.
#[test]
fn a_policy_refusal_lists_its_problems() {
    let refused = outcome_status(&RuleCommandOutcome::Refused {
        problems: vec![
            snitchwatch_bridge::rule_policy::RuleProblem {
                path: "name".into(),
                reason: "a rule with this name exists".into(),
            },
            snitchwatch_bridge::rule_policy::RuleProblem {
                path: "operator.list[1].data".into(),
                reason: "not a port".into(),
            },
        ],
    });
    assert!(!refused.saved);
    assert!(
        refused.status.starts_with("The rule wasn't sent: "),
        "{}",
        refused.status
    );
    assert!(
        refused.status.contains("not a port (condition 2's value)"),
        "{}",
        refused.status
    );
    assert!(
        refused.status.contains("a rule with this name exists"),
        "{}",
        refused.status
    );
}

#[test]
fn ok_with_a_note_is_created_and_keeps_the_note() {
    let done = outcome_status(&RuleCommandOutcome::OkWithNote {
        note: "The old file couldn't be removed.".into(),
    });
    assert!(done.saved);
    assert_eq!(
        done.status,
        "The rule was created. The old file couldn't be removed."
    );
}

#[test]
fn no_result_in_time_is_not_created() {
    let now = Instant::now();
    let mut wait = MakeRuleWait::default();
    wait.begin("make-1".into(), "1:ask-1".into(), now);
    assert!(
        wait.poll(now + NO_ANSWER_AFTER, NO_ANSWER_AFTER, |_| true)
            .is_none(),
        "not yet"
    );
    let (row, done, ending) = wait
        .poll(
            now + NO_ANSWER_AFTER + Duration::from_secs(1),
            NO_ANSWER_AFTER,
            |_| true,
        )
        .unwrap();
    assert_eq!(row, "1:ask-1");
    assert_eq!(ending, Ending::Unknown, "it may have been created");
    assert!(!done.saved);
    assert_eq!(done.status, NO_ANSWER);
    // A late Ok after giving up changes nothing.
    assert!(wait
        .on_message(&result("make-1", RuleCommandOutcome::Ok))
        .is_none());
}

/// The off-screen notice's three cases (PR #111): created only for Ok,
/// unknown when the outcome isn't known, not created otherwise.
#[test]
fn each_result_ends_as_created_not_created_or_unknown() {
    use snitchwatch_bridge::rule_policy::RuleProblem;
    let cases = [
        (RuleCommandOutcome::Ok, Ending::Created),
        (
            RuleCommandOutcome::OkWithNote { note: "n".into() },
            Ending::Created,
        ),
        (
            RuleCommandOutcome::Rejected { reason: "r".into() },
            Ending::NotCreated,
        ),
        (
            RuleCommandOutcome::Refused {
                problems: vec![RuleProblem {
                    path: "name".into(),
                    reason: "taken".into(),
                }],
            },
            Ending::NotCreated,
        ),
        (RuleCommandOutcome::NoDaemon, Ending::NotCreated),
        (RuleCommandOutcome::Timeout, Ending::Unknown),
        (
            RuleCommandOutcome::Unsure { reason: "u".into() },
            Ending::Unknown,
        ),
    ];
    for (outcome, want) in cases {
        assert_eq!(ending_of(&outcome), want, "{outcome:?}");
    }
    assert_eq!(
        [Ending::NotCreated, Ending::Created, Ending::Unknown].map(|e| e as i32),
        [0, 1, 2],
        "the numbers MakeRuleOutcomes.qml reads"
    );
}

#[test]
fn a_shorter_deadline_ends_the_wait_sooner() {
    let now = Instant::now();
    let mut wait = MakeRuleWait::default();
    wait.begin("make-1".into(), "1:ask-1".into(), now);
    let short = Duration::from_millis(50);
    assert!(wait.poll(now + short, short, |_| true).is_none());
    let (_, done, ending) = wait
        .poll(now + Duration::from_millis(51), short, |_| true)
        .unwrap();
    assert_eq!(done.status, NO_ANSWER);
    assert_eq!(ending, Ending::Unknown);
}

/// A result goes only to the connection that asked; once the row's
/// bridge session is gone it can't come, so the wait ends at once.
#[test]
fn a_reconnect_ends_the_wait_at_once() {
    let now = Instant::now();
    let mut wait = MakeRuleWait::default();
    wait.begin("make-1".into(), "3:ask-1".into(), now);
    assert!(wait
        .poll(now, NO_ANSWER_AFTER, |session| session == 3)
        .is_none());
    let (row, done, ending) = wait.poll(now, NO_ANSWER_AFTER, |_| false).unwrap();
    assert_eq!(row, "3:ask-1");
    assert_eq!(ending, Ending::Unknown);
    assert!(!done.saved);
    assert_eq!(done.status, NO_ANSWER);
    // A row id naming no session waits for the deadline only.
    wait.begin("make-2".into(), "probe-row".into(), now);
    assert!(wait.poll(now, NO_ANSWER_AFTER, |_| false).is_none());
}

#[test]
fn a_rule_that_was_not_sent_says_why() {
    use crate::bridge_runtime::SendClientMessageError as E;
    use crate::rule_commands::{NOT_CONNECTED, QUEUE_FULL};
    let message = || {
        add_rule_message(
            &deferred_row(Some("/usr/bin/curl")),
            "deny",
            "this_host",
            "forever",
            0,
            "make-1",
        )
    };
    assert_eq!(send_problem(message(), |_| Ok(())), None);
    assert_eq!(send_problem(message(), |_| Err(E::Full)), Some(QUEUE_FULL));
    assert_eq!(
        send_problem(message(), |_| Err(E::StaleSession)),
        Some(NOT_CONNECTED)
    );
    assert_eq!(
        send_problem(None, |_| panic!("nothing to send")),
        Some(NOT_SENT)
    );
}

#[test]
fn a_send_that_failed_waits_for_nothing() {
    let mut wait = MakeRuleWait::default();
    wait.begin("make-1".into(), "1:ask-1".into(), Instant::now());
    wait.abandon();
    assert!(!wait.is_waiting());
    assert!(wait
        .on_message(&result("make-1", RuleCommandOutcome::Ok))
        .is_none());
}
