//! Fan-out, destinations, ordering, non-selectable programs, Block.

use super::*;
use crate::learned::{Aggregate, PathFlag};

#[test]
fn fan_out_is_high_from_the_threshold_and_not_below_it() {
    for (count, high) in [(24, false), (25, true), (26, true)] {
        let p = one(prog_with_hosts("/opt/x/app", &hosts(count, 1)));
        assert_eq!(p.fan_out, count);
        assert_eq!(p.high_fan_out, high, "{count} destinations");
    }
}

#[test]
fn the_fan_out_threshold_is_tunable() {
    let program = prog_with_hosts("/opt/x/app", &hosts(3, 1));
    let config = ReviewConfig {
        fan_out_threshold: 3,
        ..ReviewConfig::default()
    };
    assert!(build_review(std::slice::from_ref(&program), &[], &config)[0].high_fan_out);
    assert!(!review(&[program])[0].high_fan_out);
}

#[test]
fn destinations_past_the_recorders_limit_count_towards_fan_out() {
    let mut program = prog_with_hosts("/opt/x/app", &hosts(20, 1));
    program.dest_overflow = 5;
    let p = one(program);
    assert_eq!(p.fan_out, 25);
    assert!(p.high_fan_out);
    assert!(!p.destinations.exact, "the rest are only counted");
    assert!(
        one(prog_with_hosts("/opt/x/app", &hosts(3, 1)))
            .destinations
            .exact
    );
}

#[test]
fn a_high_fan_out_program_hides_its_hosts_but_keeps_flags_and_counts() {
    let mut program = prog_with_hosts("/tmp/x/app", &hosts(30, 2));
    program.bare_ip = 3;
    program.uncommon_port = 4;
    let p = one(program);
    assert!(p.high_fan_out);
    assert!(p.destinations.hidden);
    assert!(p.destinations.top.is_empty(), "no host list by default");
    assert_eq!(p.destinations.distinct, 30);
    assert_eq!(p.destinations.connections, 60);
    assert!(p.flags.user_writable_path);
    assert_eq!(p.flags.bare_ip, (3, 60));
    assert_eq!(p.flags.uncommon_port, (4, 60));
    assert_eq!(p.describe_flags().len(), 3);
    // On demand, the busiest first.
    assert_eq!(p.top_destinations(4).len(), 4);
    assert_eq!(p.top_destinations(100).len(), 30);
    // And no per-host rule is offered for it.
    assert!(p.choice(ChoiceKind::AllowHosts).is_none());
}

#[test]
fn a_small_program_lists_its_top_destinations_busiest_first() {
    let p = one(prog(
        "app",
        "/opt/x/app",
        &[
            ("a.example", 1),
            ("b.example", 9),
            ("c.example", 4),
            ("d.example", 4),
            ("e.example", 2),
            ("f.example", 3),
        ],
    ));
    assert!(!p.destinations.hidden);
    assert_eq!(
        p.destinations.top,
        vec![
            ("b.example".to_string(), 9),
            ("c.example".to_string(), 4),
            ("d.example".to_string(), 4),
            ("f.example".to_string(), 3),
            ("e.example".to_string(), 2),
        ]
    );
    assert_eq!(p.top_destinations(1), vec![("b.example".to_string(), 9)]);
    assert_eq!(p.top_destinations(0), vec![]);
}

#[test]
fn programs_are_sorted_by_activity_then_recency_then_key() {
    let busy = prog("busy", "/opt/a/busy", &[("a.example", 50)]);
    let quiet = prog("quiet", "/opt/a/quiet", &[("a.example", 2)]);
    let mut tied_late = prog("late", "/opt/a/late", &[("a.example", 7)]);
    tied_late.last_seen_ms = 9_000;
    let tied_early = prog("early", "/opt/a/early", &[("a.example", 7)]);
    let tied_b = prog("zzz", "/opt/a/b", &[("a.example", 7)]);
    let order: Vec<String> = review(&[quiet, tied_early, busy, tied_b, tied_late])
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(
        order,
        [
            "path:/opt/a/busy",
            "path:/opt/a/late",
            "path:/opt/a/b",
            "path:/opt/a/early",
            "path:/opt/a/quiet",
        ]
    );
}

#[test]
fn each_proposal_carries_the_0a_label_marker_and_real_path() {
    let p = one(prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[("a.example", 2)],
    ));
    assert_eq!(p.name.label, "claude 2.1.308");
    assert!(p.marker, "a heuristic label is marked");
    assert_eq!(p.display, "/home/u/.local/share/claude/versions/2.1.308");
    assert_eq!(p.path_tail, "…/claude/versions/2.1.308");
    let raw = one(prog("app", "/opt/x/app", &[("a.example", 2)]));
    assert!(!raw.marker);
    assert_eq!(raw.path_tail, "/opt/x/app");
    assert_eq!(raw.connections, 2);
    assert_eq!((raw.first_seen_ms, raw.last_seen_ms), (1_000, 2_000));
}

#[test]
fn a_program_without_a_path_shows_its_name() {
    let p = one(nameless("kworker/0", &[("a.example", 2)]));
    assert_eq!(p.display, "kworker/0");
    assert_eq!(p.key, "name:kworker/0");
}

#[test]
fn programs_without_a_usable_path_are_non_selectable_and_nothing_else() {
    let cases = [
        (nameless("curl", &[("a.example", 3)]), NonSelectable::NoPath),
        (
            prog("x", "usr/bin/x", &[("a.example", 3)]),
            NonSelectable::NotBindable,
        ),
        (
            prog("x", "/proc/self/exe", &[("a.example", 3)]),
            NonSelectable::NotBindable,
        ),
        (
            prog("x", "/memfd:x", &[("a.example", 3)]),
            NonSelectable::NotBindable,
        ),
        (
            prog("x", "/usr/../bin/x", &[("a.example", 3)]),
            NonSelectable::NotBindable,
        ),
        (
            prog("x", "/usr//bin/x", &[("a.example", 3)]),
            NonSelectable::NotBindable,
        ),
        (
            prog("x", "/usr/bin/x (deleted)", &[("a.example", 3)]),
            NonSelectable::Deleted,
        ),
        (
            prog("x", "/usr/bin/x(deleted)", &[("a.example", 3)]),
            NonSelectable::Deleted,
        ),
        (
            prog("x", "/home/u/x\ny", &[("a.example", 3)]),
            NonSelectable::Truncated,
        ),
        (
            Program {
                truncated: true,
                ..prog("x", "/usr/bin/x", &[("a.example", 3)])
            },
            NonSelectable::Truncated,
        ),
        (
            prog(
                "x",
                &format!("/opt/{}", "a".repeat(2000)),
                &[("a.example", 3)],
            ),
            NonSelectable::Truncated,
        ),
    ];
    for (program, why) in cases {
        let path = program.raw_path.clone();
        let p = one(program);
        assert_eq!(p.choices, vec![Choice::NonSelectable(why)], "{path:?}");
        assert!(!p.selected_by_suggest, "{path:?}");
        assert!(
            p.block_warning.is_none(),
            "{path:?}: no Block, so no warning"
        );
        assert!(p.already_present.is_empty());
        assert!(!why.text().is_empty());
    }
}

#[test]
fn a_non_selectable_program_still_shows_its_flags() {
    let mut program = prog("x", "/tmp/x (deleted)", &[("a.example", 3)]);
    program.bare_ip = 2;
    let p = one(program);
    assert_eq!(p.flags.path_flag, Some(PathFlag::Deleted));
    assert!(!p.describe_flags().is_empty());
}

#[test]
fn a_selectable_program_can_be_allowed_anywhere_or_blocked() {
    let p = one(prog("app", "/opt/x/app", &[("a.example", 1)]));
    assert_eq!(p.choices, vec![Choice::AllowAny, Choice::Block]);
    let p = one(prog("app", "/opt/x/app", &[("a.example", 2)]));
    assert_eq!(
        p.choices,
        vec![
            Choice::AllowHosts(vec!["a.example".to_string()]),
            Choice::AllowAny,
            Choice::Block
        ]
    );
}

#[test]
fn block_warns_for_system_paths_and_system_services_only() {
    let warned = [
        prog("curl", "/usr/bin/curl", &[("a.example", 1)]),
        prog("x", "/bin/x", &[("a.example", 1)]),
        prog("x", "/usr/sbin/x", &[("a.example", 1)]),
        prog("x", "/usr/local/bin/x", &[("a.example", 1)]),
        prog("x", "/lib64/x", &[("a.example", 1)]),
        prog("NetworkManager", "/opt/x/nm", &[("a.example", 1)]),
        prog("systemd-resolved", "/opt/x/r", &[("a.example", 1)]),
        prog("dbus-daemon", "/opt/x/d", &[("a.example", 1)]),
        prog("chronyd", "/opt/x/c", &[("a.example", 1)]),
        prog("rpm-ostree", "/opt/x/r", &[("a.example", 1)]),
        prog("x", "/opt/x/NetworkManager", &[("a.example", 1)]),
        prog("kworker/0:1", "/opt/x/w", &[("a.example", 1)]),
    ];
    for program in warned {
        let path = program.raw_path.clone();
        let p = one(program);
        assert_eq!(
            p.block_warning.as_deref(),
            Some("Blocking this may stop networking or updates."),
            "{path:?}"
        );
        assert!(p.choice(ChoiceKind::Block).is_some());
    }
    for program in [
        prog("app", "/opt/vendor/app", &[("a.example", 1)]),
        prog("app", "/home/u/bin/app", &[("a.example", 1)]),
        prog("systemdish", "/opt/x/systemdish", &[("a.example", 1)]),
        prog("usr", "/opt/usr/bin/x", &[("a.example", 1)]),
    ] {
        let path = program.raw_path.clone();
        assert!(one(program).block_warning.is_none(), "{path:?}");
    }
}

#[test]
fn block_is_never_selected_and_exists_for_every_selectable_program() {
    for path in ["/usr/bin/a", "/opt/a", "/tmp/a", "/home/u/a"] {
        let p = one(prog("app", path, &[("a.example", 5)]));
        assert!(p.choice(ChoiceKind::Block).is_some(), "{path}");
        assert_ne!(
            p.suggested_choice().map(Choice::kind),
            Some(ChoiceKind::Block)
        );
    }
}

#[test]
fn the_aggregate_is_flattened_from_all_four_classes() {
    let mut agg = Aggregate::default();
    let mut network = prog("n", "/opt/n/n", &[("1.2.3.4", 1)]);
    network.bare_ip = 1;
    for program in [
        prog("c", "/usr/bin/c", &[("a.example", 1)]),
        prog("b", "/tmp/b", &[("a.example", 1)]),
        nameless("d", &[("a.example", 1)]),
        network,
    ] {
        agg.absorb(program);
    }
    let mut keys: Vec<String> = programs_of(&agg).into_iter().map(|p| p.key).collect();
    keys.sort();
    assert_eq!(
        keys,
        ["name:d", "path:/opt/n/n", "path:/tmp/b", "path:/usr/bin/c"]
    );
}

#[test]
fn building_a_review_leaves_its_input_alone() {
    let programs = vec![prog("app", "/usr/bin/app", &[("a.example", 2)])];
    let before = programs.clone();
    let _ = review(&programs);
    assert_eq!(programs, before);
}
