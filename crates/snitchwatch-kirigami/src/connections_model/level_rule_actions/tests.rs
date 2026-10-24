use super::*;
use crate::connections::grouping::GroupCounts;

const PROG: &str = "/usr/bin/curl";

fn shown() -> Vec<VisibleEntry> {
    let counts = GroupCounts::default();
    vec![
        VisibleEntry::ProcessHeader {
            key: PROG.into(),
            label: "curl".into(),
            expanded: true,
            counts,
        },
        VisibleEntry::DomainHeader {
            process_key: PROG.into(),
            key: "github.com".into(),
            label: "github.com".into(),
            expanded: true,
            counts,
        },
        VisibleEntry::HostHeader {
            process_key: PROG.into(),
            domain_key: "github.com".into(),
            key: "api.github.com".into(),
            label: "api.github.com".into(),
            expanded: false,
            counts,
        },
        VisibleEntry::Row {
            id: "1".into(),
            process_key: PROG.into(),
            domain_key: "github.com".into(),
            host_key: None,
        },
    ]
}

fn node<'a>(program: &'a str, level: Level, key: &'a str) -> NodeRef<'a> {
    NodeRef {
        program,
        level,
        key,
    }
}

#[test]
fn a_node_the_projection_shows_is_known() {
    let entries = shown();
    for n in [
        node(PROG, Level::Program, ""),
        node(PROG, Level::Domain, "github.com"),
        node(PROG, Level::Host, "api.github.com"),
    ] {
        assert_eq!(known_node(true, &entries, n), Ok(()), "{n:?}");
    }
}

#[test]
fn a_node_the_projection_does_not_show_is_refused() {
    let entries = shown();
    for n in [
        node(PROG, Level::Domain, "example.com"),
        node(PROG, Level::Host, "github.com"),
        node("/usr/bin/wget", Level::Program, ""),
        node(PROG, Level::Program, "github.com"),
        node(PROG, Level::Domain, ""),
    ] {
        assert_eq!(known_node(true, &entries, n), Err(NOT_IN_LIST), "{n:?}");
    }
    assert_eq!(
        known_node(true, &[], node(PROG, Level::Program, "")),
        Err(NOT_IN_LIST)
    );
}

#[test]
fn empty_and_oversized_keys_are_refused_even_when_a_node_matches() {
    let long = "a".repeat(MAX_KEY_BYTES + 1);
    let counts = GroupCounts::default();
    let entries = vec![
        VisibleEntry::DomainHeader {
            process_key: PROG.into(),
            key: long.clone(),
            label: String::new(),
            expanded: false,
            counts,
        },
        VisibleEntry::DomainHeader {
            process_key: PROG.into(),
            key: String::new(),
            label: String::new(),
            expanded: false,
            counts,
        },
    ];
    assert_eq!(
        known_node(true, &entries, node(PROG, Level::Domain, &long)),
        Err(NOT_IN_LIST)
    );
    assert_eq!(
        known_node(true, &entries, node(PROG, Level::Domain, "")),
        Err(NOT_IN_LIST)
    );
    let exact = "a".repeat(MAX_KEY_BYTES);
    let entries = vec![VisibleEntry::HostHeader {
        process_key: PROG.into(),
        domain_key: "x".into(),
        key: exact.clone(),
        label: String::new(),
        expanded: false,
        counts,
    }];
    assert_eq!(
        known_node(true, &entries, node(PROG, Level::Host, &exact)),
        Ok(())
    );
    assert_eq!(
        known_node(true, &entries, node("", Level::Host, &exact)),
        Err(NOT_IN_LIST)
    );
}

fn projection(state: RuleState, names: &[&str], matching: usize) -> Projection {
    Projection {
        state,
        names: names.iter().map(|n| n.to_string()).collect(),
        matching,
        effective: Level::Host,
        too_broad: false,
        reason: None,
    }
}

#[test]
fn a_single_deletable_rule_is_the_one_to_delete() {
    assert_eq!(
        delete_name(&projection(RuleState::Allow, &["r1"], 1)),
        Ok("r1".to_owned())
    );
    assert_eq!(
        delete_name(&projection(RuleState::Deny, &["r2"], 1)),
        Ok("r2".to_owned())
    );
}

#[test]
fn a_delete_with_duplicate_rules_points_at_the_rules_page() {
    for state in [RuleState::Allow, RuleState::Deny, RuleState::Partial] {
        let reason = delete_name(&projection(state, &["a", "b"], 2)).unwrap_err();
        assert!(reason.contains("Rules page"), "{state:?}: {reason}");
    }
    // A second rule Snitchwatch can't delete still counts as a duplicate.
    let reason = delete_name(&projection(RuleState::Allow, &["a"], 2)).unwrap_err();
    assert!(reason.contains("more than one"), "{reason}");
}

#[test]
fn a_delete_with_nothing_or_something_in_flight_is_refused() {
    assert_eq!(
        delete_name(&projection(RuleState::Pending, &["a"], 1)),
        Err(ALREADY_WORKING)
    );
    let reason = delete_name(&projection(RuleState::Partial, &[], 0)).unwrap_err();
    assert!(reason.contains("Rules page"), "{reason}");
    let reason = delete_name(&projection(RuleState::Allow, &[], 1)).unwrap_err();
    assert!(reason.contains("Rules page"), "{reason}");
    assert!(delete_name(&projection(RuleState::None, &[], 0)).is_err());
}

#[test]
fn a_node_with_no_rule_is_unavailable_when_the_duration_is_once_only() {
    let none = projection(RuleState::None, &[], 0);
    assert_eq!(
        shown_state(&none, || false),
        (
            RuleState::Unavailable,
            Some(LevelRuleRefusal::NotRemembered)
        )
    );
    assert_eq!(shown_state(&none, || true), (RuleState::None, None));
}

#[test]
fn an_existing_rule_stays_removable_whatever_the_duration() {
    for state in [RuleState::Allow, RuleState::Deny, RuleState::Partial] {
        let p = projection(state, &["r"], 1);
        // The duration is not even asked for.
        assert_eq!(shown_state(&p, || unreachable!()), (state, None));
    }
}

#[test]
fn an_unavailable_projection_keeps_its_own_reason() {
    let mut p = projection(RuleState::Unavailable, &[], 0);
    p.reason = Some(LevelRuleRefusal::TooBroad);
    assert_eq!(
        shown_state(&p, || false),
        (RuleState::Unavailable, Some(LevelRuleRefusal::TooBroad))
    );
}

#[test]
fn a_flat_model_refuses_every_node_whatever_the_cached_projection_holds() {
    let entries = shown();
    let n = node(PROG, Level::Domain, "github.com");
    assert_eq!(known_node(true, &entries, n), Ok(()));
    assert_eq!(known_node(false, &entries, n), Err(NOT_IN_LIST));
}

#[test]
fn the_duration_is_read_once_however_often_it_is_asked() {
    let reads = Cell::new(0);
    let cache = DurationCache::default();
    let read = || {
        reads.set(reads.get() + 1);
        "this_time"
    };
    for _ in 0..1000 {
        assert_eq!(cache.get(read), "this_time");
    }
    assert_eq!(reads.get(), 1);
}

#[test]
fn a_refresh_reads_again_and_says_whether_the_duration_changed() {
    let cache = DurationCache::default();
    assert_eq!(cache.get(|| "forever"), "forever");
    assert!(!cache.refresh(|| "forever"), "same token is no change");
    assert!(cache.refresh(|| "this_time"), "a new token is a change");
    // The new token is what is held, with no further read.
    assert_eq!(cache.get(|| panic!("read after a refresh")), "this_time");
}

#[test]
fn a_refresh_before_any_read_counts_as_a_change() {
    assert!(DurationCache::default().refresh(|| "forever"));
}

#[test]
fn a_shown_duration_must_still_be_the_current_one() {
    assert_eq!(duration_matches("forever", "forever"), Ok(()));
    assert_eq!(duration_matches("", "this_time"), Ok(()), "none was shown");
    assert_eq!(
        duration_matches("for_5_minutes", "forever"),
        Err(DURATION_CHANGED)
    );
}

#[test]
fn deletion_requires_the_rule_name_and_state_shown_in_the_menu() {
    let replacement = projection(RuleState::Allow, &["rule-B"], 1);
    assert_eq!(
        checked_delete_name(&replacement, Some(("rule-A", "allow"))),
        Err(RULE_CHANGED)
    );
    assert_eq!(
        checked_delete_name(&replacement, Some(("rule-B", "deny"))),
        Err(RULE_CHANGED)
    );
    assert_eq!(
        checked_delete_name(&replacement, Some(("rule-B", "allow"))),
        Ok("rule-B".into())
    );
    assert_eq!(
        checked_delete_name(&replacement, Some(("", "allow"))),
        Err(RULE_CHANGED)
    );
}

#[test]
fn menu_session_tokens_must_match_the_model_snapshot() {
    for token in ["", "0:", "1", "1::", "other:", "2:"] {
        assert_eq!(
            captured_session(Some(1), &QString::from(token)),
            Err(SESSION_CHANGED),
            "{token}"
        );
    }
    assert_eq!(
        captured_session(None, &QString::from("1:")),
        Err(SESSION_CHANGED)
    );
    assert_eq!(captured_session(Some(1), &QString::from("1:")), Ok(1));
}
