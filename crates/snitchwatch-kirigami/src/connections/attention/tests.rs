use super::*;

const SSHD: &str = "/usr/sbin/sshd";
const RECOMMENDED: &str = "snitchwatch-default-chronyd";

fn rule(name: &str, enabled: bool) -> Rule {
    Rule {
        name: name.to_string(),
        enabled,
        action: "allow".to_string(),
        toggleable: Some(true),
        ..Rule::default()
    }
}

/// A row every clause accepts: allowed by an enabled recommended rule, for a
/// program seen before. Each test changes exactly one thing.
fn handled_row() -> ConnectionRow {
    ConnectionRow {
        id: "r1".to_string(),
        process: "sshd".to_string(),
        process_path: Some(SSHD.to_string()),
        dst_host: "pool.ntp.org".to_string(),
        dst_ip: "10.0.0.1".to_string(),
        dst_port: 123,
        protocol: "udp".to_string(),
        direction: "outgoing".to_string(),
        action: Some("allow".to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        matched_rule: Some(RECOMMENDED.to_string()),
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

fn facts() -> AttentionFacts {
    AttentionFacts::new(&[rule(RECOMMENDED, true)], [SSHD])
}

#[test]
fn a_seen_program_allowed_by_an_enabled_recommended_rule_is_handled() {
    assert!(facts().collapses_anything());
    assert!(facts().is_handled(&handled_row()));
}

#[test]
fn a_pending_row_is_never_handled() {
    let row = ConnectionRow {
        action: None,
        ..handled_row()
    };
    assert!(!facts().is_handled(&row));
}

#[test]
fn a_deferred_row_is_never_handled_even_with_an_allow_action() {
    let row = ConnectionRow {
        deferred: true,
        ..handled_row()
    };
    assert!(!facts().is_handled(&row));
}

#[test]
fn a_deferred_row_without_an_action_is_never_handled() {
    let row = ConnectionRow {
        deferred: true,
        action: None,
        ..handled_row()
    };
    assert!(!facts().is_handled(&row));
}

#[test]
fn a_default_decided_row_is_never_handled_whatever_rule_it_names() {
    let row = ConnectionRow {
        decided_by_default: true,
        ..handled_row()
    };
    assert!(!facts().is_handled(&row));
}

#[test]
fn an_allowed_row_with_no_matched_rule_is_default_decided_and_never_handled() {
    let row = ConnectionRow {
        matched_rule: None,
        ..handled_row()
    };
    assert!(!facts().is_handled(&row));
    let blank = ConnectionRow {
        matched_rule: Some(String::new()),
        ..handled_row()
    };
    assert!(!facts().is_handled(&blank));
}

#[test]
fn denied_blocklisted_and_unknown_action_rows_are_never_handled() {
    for action in ["deny", "blocklist", "reject", "ALLOW", ""] {
        let row = ConnectionRow {
            action: Some(action.to_string()),
            ..handled_row()
        };
        assert!(!facts().is_handled(&row), "{action}");
    }
}

#[test]
fn a_stale_rule_name_that_is_no_longer_in_the_rules_list_is_not_handled() {
    let facts = AttentionFacts::new(&[rule("snitchwatch-default-other", true)], [SSHD]);
    assert!(facts.collapses_anything());
    assert!(!facts.is_handled(&handled_row()));
}

#[test]
fn a_disabled_recommended_rule_does_not_handle_its_rows() {
    let facts = AttentionFacts::new(&[rule(RECOMMENDED, false)], [SSHD]);
    assert!(!facts.collapses_anything());
    assert!(!facts.is_handled(&handled_row()));
}

#[test]
fn a_user_rule_never_handles_a_row_even_a_host_only_one() {
    let name = "allow-ntp-host";
    let facts = AttentionFacts::new(&[rule(RECOMMENDED, true), rule(name, true)], [SSHD]);
    let row = ConnectionRow {
        matched_rule: Some(name.to_string()),
        ..handled_row()
    };
    assert!(!facts.is_handled(&row));
}

#[test]
fn a_rule_merely_containing_the_prefix_is_not_recommended() {
    let name = "x-snitchwatch-default-chronyd";
    let facts = AttentionFacts::new(&[rule(RECOMMENDED, true), rule(name, true)], [SSHD]);
    let row = ConnectionRow {
        matched_rule: Some(name.to_string()),
        ..handled_row()
    };
    assert!(!facts.is_handled(&row));
}

#[test]
fn a_once_off_answer_never_handles_a_row() {
    let name = "snitchwatch-allow-once-sshd";
    let facts = AttentionFacts::new(&[rule(RECOMMENDED, true), rule(name, true)], [SSHD]);
    let row = ConnectionRow {
        matched_rule: Some(name.to_string()),
        ..handled_row()
    };
    assert!(!facts.is_handled(&row));
}

#[test]
fn a_first_seen_program_is_an_exception() {
    let facts = AttentionFacts::new(&[rule(RECOMMENDED, true)], ["/usr/bin/other"]);
    assert!(!facts.is_handled(&handled_row()));
}

#[test]
fn a_row_without_a_process_path_is_never_handled() {
    let row = ConnectionRow {
        process_path: None,
        ..handled_row()
    };
    assert!(!facts().is_handled(&row));
    // Not even when its display name happens to equal a seen path.
    let named = ConnectionRow {
        process_path: None,
        process: SSHD.to_string(),
        ..handled_row()
    };
    assert!(!facts().is_handled(&named));
}

#[test]
fn with_no_rules_nothing_is_handled_and_nothing_collapses() {
    let facts = AttentionFacts::new(&[], [SSHD]);
    assert!(!facts.collapses_anything());
    assert!(!facts.is_handled(&handled_row()));
    assert!(!AttentionFacts::default().is_handled(&handled_row()));
}

#[test]
fn handled_counts_follow_each_row_delta() {
    let facts = facts();
    let mut counts = HandledCounts::default();
    counts.upsert(Some(&facts), &handled_row());
    assert_eq!(counts.rows(), 0, "off until rebuilt on");
    counts.rebuild(true, Some(&facts), &[handled_row()]);
    assert_eq!((counts.rows(), counts.programs()), (1, 1));
    let second = ConnectionRow {
        id: "r2".to_string(),
        ..handled_row()
    };
    counts.upsert(Some(&facts), &second);
    counts.upsert(Some(&facts), &second);
    assert_eq!(
        (counts.rows(), counts.programs()),
        (2, 1),
        "an upsert counts once"
    );
    let denied = ConnectionRow {
        action: Some("deny".to_string()),
        ..second.clone()
    };
    counts.upsert(Some(&facts), &denied);
    assert_eq!(counts.rows(), 1, "a row that stops being handled leaves");
    counts.remove("r1");
    assert_eq!((counts.rows(), counts.programs()), (0, 0));
    counts.upsert(None, &handled_row());
    assert_eq!(counts.rows(), 0, "no facts, nothing handled");
    counts.rebuild(false, Some(&facts), &[handled_row()]);
    assert_eq!(counts.rows(), 0, "off does no work");
}

#[test]
fn a_prefixed_rule_the_bridge_does_not_mark_toggleable_is_a_user_rule() {
    for toggleable in [Some(false), None] {
        let named = Rule {
            toggleable,
            ..rule(RECOMMENDED, true)
        };
        let facts = AttentionFacts::new(&[named], [SSHD]);
        assert!(!facts.collapses_anything(), "{toggleable:?}");
        assert!(!facts.is_handled(&handled_row()), "{toggleable:?}");
    }
}
