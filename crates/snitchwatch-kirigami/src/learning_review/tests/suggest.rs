//! "Select suggested", the host rules and what is ticked by default.

use super::*;

fn twice(path: &str) -> Program {
    prog("app", path, &[("a.example", 2), ("b.example", 3)])
}

#[test]
fn nothing_is_ticked_by_default_unless_a_narrow_rule_is_suggested() {
    // A varied set: only the suggested field can ask for a tick, and it is
    // set for none of the programs that are not plainly narrow and unflagged.
    let mut flagged = twice("/usr/bin/net");
    flagged.uncommon_port = 1;
    let programs = [
        twice("/opt/x/app"),
        twice("/tmp/app"),
        twice("/usr/local/bin/app"),
        flagged,
        nameless("x", &[("a.example", 3)]),
        twice("/usr/bin/python3"),
        prog_with_hosts("/usr/bin/many", &hosts(30, 2)),
    ];
    for p in review(&programs) {
        assert!(!p.selected_by_suggest, "{}", p.key);
        assert!(p.suggested_choice().is_none(), "{}", p.key);
    }
}

#[test]
fn suggest_selects_the_host_rules_of_an_unflagged_direct_system_program() {
    for path in [
        "/usr/bin/app",
        "/usr/sbin/app",
        "/usr/libexec/app",
        "/usr/lib/app",
        "/usr/lib64/app",
        "/usr/lib32/app",
    ] {
        let p = one(twice(path));
        assert!(p.selected_by_suggest, "{path}");
        assert_eq!(
            p.suggested_choice(),
            Some(&Choice::AllowHosts(vec![
                "b.example".to_string(),
                "a.example".to_string()
            ])),
            "{path}"
        );
    }
}

#[test]
fn only_a_file_directly_under_a_system_directory_qualifies() {
    for path in [
        "/usr/lib/firefox/firefox",
        "/usr/libexec/sub/app",
        "/usr/bin/sub/app",
        "/usr/local/bin/app",
        "/usr/local/sbin/app",
        "/usr/share/app/app",
        "/usr/app",
        "/bin/app",
        "/sbin/app",
        "/opt/app/bin/app",
        "/opt/app",
        "/var/lib/flatpak/exports/bin/app",
        "/var/lib/flatpak/app/org.x/current/active/files/bin/app",
        "/home/u/.local/share/flatpak/exports/bin/app",
        "/home/u/.local/share/flatpak/app/org.x/current/active/files/bin/app",
        "/usr/library/app",
        "/usr/bin",
        "/usr/lib/",
    ] {
        let p = one(twice(path));
        assert!(!p.selected_by_suggest, "{path}");
    }
}

#[test]
fn a_flag_withholds_the_suggestion() {
    let mut bare = twice("/usr/bin/app");
    bare.bare_ip = 1;
    let mut port = twice("/usr/bin/app");
    port.uncommon_port = 1;
    for program in [bare, port] {
        let p = one(program);
        assert!(p.flags.any());
        assert!(!p.selected_by_suggest);
        // The narrow rule is still an explicit choice.
        assert!(p.choice(ChoiceKind::AllowHosts).is_some());
    }
}

#[test]
fn a_name_that_mimics_a_system_program_is_never_suggested() {
    let p = one(prog("sshd", "/usr/local/bin/sshd", &[("a.example", 2)]));
    assert!(p.flags.name_mimics_system);
    assert!(!p.selected_by_suggest);
}

#[test]
fn a_dual_use_program_is_never_suggested() {
    for path in [
        "/usr/bin/python3",
        "/usr/bin/curl",
        "/usr/bin/bash",
        "/usr/bin/git",
    ] {
        let p = one(twice(path));
        assert!(p.dual_use, "{path}");
        assert!(!p.selected_by_suggest, "{path}");
        assert!(p.choice(ChoiceKind::AllowHosts).is_some(), "{path}");
    }
}

#[test]
fn a_renamed_interpreter_copy_outside_the_system_directories_stays_unselected() {
    // Not on the list by name, so the PATH rule is what keeps it unselected.
    let copy = one(twice("/usr/local/bin/mytool"));
    assert!(!copy.dual_use);
    assert!(!copy.selected_by_suggest);
}

#[test]
fn a_high_fan_out_program_is_never_suggested() {
    let config = ReviewConfig {
        fan_out_threshold: 2,
        ..ReviewConfig::default()
    };
    let p = &build_review(&[twice("/usr/bin/app")], &[], &config)[0];
    assert!(p.high_fan_out);
    assert!(!p.selected_by_suggest);
}

#[test]
fn suggest_never_picks_any_destination_block_or_a_directory_rule() {
    let versioned = prog(
        "2.1.308",
        "/usr/lib/tool/versions/2.1.308",
        &[("a.example", 2)],
    );
    let programs = [
        twice("/usr/bin/app"),
        twice("/usr/bin/python3"),
        twice("/usr/lib/x"),
        versioned,
        prog("app", "/usr/bin/once", &[("a.example", 1)]),
        nameless("x", &[("a.example", 5)]),
    ];
    for p in review(&programs) {
        if let Some(choice) = p.suggested_choice() {
            assert_eq!(choice.kind(), ChoiceKind::AllowHosts, "{}", p.key);
        }
        assert_eq!(
            p.selected_by_suggest,
            p.key == "path:/usr/bin/app" || p.key == "path:/usr/lib/x",
            "{}",
            p.key
        );
    }
}

#[test]
fn a_directory_rule_under_a_user_directory_is_offered_but_not_suggested() {
    let p = one(prog(
        "2.1.308",
        "/home/u/.local/share/claude/versions/2.1.308",
        &[("a.example", 2)],
    ));
    assert!(p.choice(ChoiceKind::DirectoryRule).is_some());
    assert!(!p.selected_by_suggest);
}

#[test]
fn the_decision_does_not_depend_on_what_the_program_is_called() {
    // A desktop entry (or any name) is never an input: only the path, the
    // flags and the counts decide. Same facts, different names, same answer.
    for name in ["app", "firefox", "Firefox", "org.mozilla.firefox", "gjs"] {
        let named = one(prog(name, "/opt/mozilla/firefox", &[("a.example", 2)]));
        assert!(!named.selected_by_suggest, "{name}");
        assert_eq!(named.choices.len(), 3, "{name}");
        let system = one(prog(name, "/usr/bin/app", &[("a.example", 2)]));
        assert!(system.selected_by_suggest, "{name}");
    }
}

// ---- the hosts of "only these destinations" --------------------------------

fn offered_hosts(p: &ProgramProposal) -> Option<Vec<String>> {
    match p.choice(ChoiceKind::AllowHosts) {
        Some(Choice::AllowHosts(h)) => Some(h.clone()),
        _ => None,
    }
}

#[test]
fn a_host_needs_two_sightings_to_be_listed() {
    let p = one(prog(
        "app",
        "/opt/x/app",
        &[
            ("once.example", 1),
            ("twice.example", 2),
            ("many.example", 9),
        ],
    ));
    assert_eq!(
        offered_hosts(&p),
        Some(vec![
            "many.example".to_string(),
            "twice.example".to_string()
        ])
    );
    let none = one(prog(
        "app",
        "/opt/x/app",
        &[("once.example", 1), ("again.example", 1)],
    ));
    assert_eq!(
        offered_hosts(&none),
        None,
        "no host qualifies: no host rule"
    );
    assert!(!none.selected_by_suggest);
}

#[test]
fn the_sighting_rule_is_tunable() {
    let program = prog("app", "/opt/x/app", &[("a.example", 2), ("b.example", 3)]);
    let config = ReviewConfig {
        min_host_sightings: 3,
        ..ReviewConfig::default()
    };
    let p = &build_review(&[program], &[], &config)[0];
    assert_eq!(offered_hosts(p), Some(vec!["b.example".to_string()]));
}

#[test]
fn ten_hosts_are_offered_and_eleven_are_not() {
    let ten = one(prog_with_hosts("/usr/bin/app", &hosts(10, 2)));
    assert_eq!(offered_hosts(&ten).map(|h| h.len()), Some(10));
    assert!(ten.selected_by_suggest);
    let eleven = one(prog_with_hosts("/usr/bin/app", &hosts(11, 2)));
    assert_eq!(offered_hosts(&eleven), None, "never a silent cut to ten");
    assert!(!eleven.selected_by_suggest);
    assert!(eleven.choice(ChoiceKind::AllowAny).is_some());
}

#[test]
fn the_host_limit_counts_only_hosts_that_qualify() {
    let mut list = hosts(10, 2);
    list.extend((0..8).map(|i| (format!("once{i}.example"), 1)));
    let p = one(prog_with_hosts("/usr/bin/app", &list));
    assert_eq!(offered_hosts(&p).map(|h| h.len()), Some(10));
}

#[test]
fn the_host_limit_is_tunable() {
    let config = ReviewConfig {
        max_hosts: 2,
        ..ReviewConfig::default()
    };
    let three = build_review(&[prog_with_hosts("/opt/x/app", &hosts(3, 2))], &[], &config);
    assert_eq!(offered_hosts(&three[0]), None);
    let two = build_review(&[prog_with_hosts("/opt/x/app", &hosts(2, 2))], &[], &config);
    assert_eq!(offered_hosts(&two[0]).map(|h| h.len()), Some(2));
}

#[test]
fn a_dual_use_program_with_no_qualifying_host_gets_no_host_rule() {
    let p = one(prog("python3", "/usr/bin/python3", &[("a.example", 1)]));
    assert!(p.dual_use);
    assert_eq!(offered_hosts(&p), None);
    assert!(!p.selected_by_suggest);
}

#[test]
fn a_dual_use_program_keeps_any_destination_and_block_as_explicit_choices() {
    let p = one(prog("python3", "/usr/bin/python3", &[("a.example", 3)]));
    assert_eq!(
        p.choices.iter().map(Choice::kind).collect::<Vec<_>>(),
        [
            ChoiceKind::AllowHosts,
            ChoiceKind::AllowAny,
            ChoiceKind::Block
        ]
    );
    assert!(
        p.allow_any_warning.is_some(),
        "any destination is warned about"
    );
    assert!(one(prog("app", "/usr/bin/app", &[("a.example", 3)]))
        .allow_any_warning
        .is_none());
}

#[test]
fn hosts_that_cannot_be_a_rule_are_left_out() {
    let p = one(prog(
        "app",
        "/opt/x/app",
        &[
            ("ok.example", 2),
            ("has space.example", 2),
            ("tab\there", 2),
        ],
    ));
    // `normalized` turns the tab into `_`; a space stays and is refused.
    let hosts = offered_hosts(&p).unwrap();
    assert!(hosts.contains(&"ok.example".to_string()));
    assert!(!hosts.iter().any(|h| h.contains(' ')));
}

#[test]
fn a_bare_ip_destination_is_listed_as_an_address() {
    let mut program = prog(
        "app",
        "/opt/x/app",
        &[("203.0.113.9", 2), ("ok.example", 2)],
    );
    program.bare_ip = 2;
    let p = one(program);
    assert_eq!(
        offered_hosts(&p).map(|h| h.contains(&"203.0.113.9".to_string())),
        Some(true)
    );
    assert!(p.flags.any());
}
