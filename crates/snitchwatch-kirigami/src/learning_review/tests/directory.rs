//! The versioned-binary directory rule: escaping, anchoring, wording, and
//! the pattern read by the GUI's RE2 simulator and the bridge's own policy.

use super::*;
use crate::rules::row_store::RulesStore;
use crate::rules::simulator::{simulate, SimulationInput};
use snitchwatch_bridge::rule_policy::{check_wire_rule, PolicyProfile};
use snitchwatch_bridge::ws_messages::ServerMessage;

const CLAUDE: &str = "/home/u/.local/share/claude/versions/2.1.308";

fn rule_of(path: &str) -> Option<DirectoryRule> {
    directory_rule(&prog("2.1.308", path, &[("a.example", 2)]))
}

fn payload_of(path: &str) -> RulePayload {
    let program = prog("2.1.308", path, &[("a.example", 2)]);
    let rule = directory_rule(&program).unwrap();
    rules_for(&Choice::DirectoryRule(rule), &program)
        .unwrap()
        .remove(0)
}

/// Whether the daemon would match `candidate` with the rule made for `path`,
/// read by the GUI's own simulator (Go's `regexp` rules, case-sensitive).
fn matches(rule_path: &str, candidate: &str) -> bool {
    let payload = payload_of(rule_path);
    let mut store = RulesStore::new();
    store.apply(&ServerMessage::SetRules {
        rules: vec![payload.to_wire()],
    });
    let input = SimulationInput {
        process_path: Some(candidate.to_string()),
        dest_host: Some("example.com".to_string()),
        dest_port: 443,
        protocol: Some("tcp".to_string()),
        ..Default::default()
    };
    let result = simulate(&store, &input);
    assert!(
        result.unsupported_operands.is_empty(),
        "{:?}",
        result.unsupported_operands
    );
    assert!(result.unevaluated.is_empty(), "{:?}", result.unevaluated);
    result.matched_rule.is_some()
}

#[test]
fn the_rule_is_an_escaped_anchored_case_sensitive_process_path_pattern() {
    let rule = rule_of(CLAUDE).unwrap();
    assert_eq!(
        rule.pattern,
        r"^/home/u/\.local/share/claude/versions/[^/]*[^/)]$"
    );
    assert!(rule.sensitive);
    assert_eq!(rule.folder, "/home/u/.local/share/claude/versions/");
    assert!(rule.user_writable);
    assert_eq!(
        rule.text(),
        "ANY file in this folder: /home/u/.local/share/claude/versions/ (a folder your user can write to)"
    );
}

#[test]
fn a_folder_the_user_cannot_write_to_is_not_called_one() {
    let rule = rule_of("/opt/tool/versions/1.2.3").unwrap();
    assert!(!rule.user_writable);
    assert_eq!(rule.text(), "ANY file in this folder: /opt/tool/versions/");
    for path in [
        "/var/home/u/x/app/versions/1.2",
        "/tmp/app/versions/1.2",
        "/root/app/versions/1.2",
    ] {
        assert!(rule_of(path).unwrap().user_writable, "{path}");
    }
}

#[test]
fn the_pattern_matches_files_in_the_folder_and_nothing_else() {
    let m = |c: &str| matches(CLAUDE, c);
    assert!(m(CLAUDE));
    assert!(m("/home/u/.local/share/claude/versions/2.1.309"));
    assert!(m("/home/u/.local/share/claude/versions/anything-else"));
    // A further `/` after the version segment.
    assert!(!m("/home/u/.local/share/claude/versions/2.1.308/evil"));
    assert!(!m("/home/u/.local/share/claude/versions/2.1.308/"));
    assert!(!m("/home/u/.local/share/claude/versions/"));
    // Anchored at both ends.
    assert!(!m("/x/home/u/.local/share/claude/versions/2.1.308"));
    // Other folders, and the dot is a dot.
    assert!(!m("/home/u/.local/share/claude/versions2/2.1.308"));
    assert!(!m("/home/u/XlocalXshare/claude/versions/2.1.308"));
    assert!(!m("/home/u/.local/share/other/versions/2.1.308"));
    // Case matters on Linux paths.
    assert!(!m("/home/u/.local/share/Claude/versions/2.1.308"));
}

#[test]
fn a_deleted_file_in_the_folder_is_not_matched() {
    let m = |c: &str| matches(CLAUDE, c);
    assert!(!m("/home/u/.local/share/claude/versions/2.1.308 (deleted)"));
    assert!(!m("/home/u/.local/share/claude/versions/2.1.308(deleted)"));
    assert!(m("/home/u/.local/share/claude/versions/2.1.308 beta"));
}

#[test]
fn a_program_that_is_a_deleted_file_is_offered_no_directory_rule() {
    assert_eq!(
        rule_of("/home/u/.local/share/claude/versions/2.1.308 (deleted)"),
        None
    );
}

#[test]
fn only_the_versions_shape_gets_a_directory_rule() {
    for path in [
        "/usr/bin/curl",
        "/home/u/app/versions/latest",
        "/home/u/app/releases/1.2.3",
        "/home/u/bin/versions/1.2.3",
        "/home/u/app/versions/1.2.3/bin/app",
    ] {
        assert_eq!(rule_of(path), None, "{path}");
    }
    assert!(directory_rule(&nameless("2.1.308", &[])).is_none());
    let truncated = Program {
        truncated: true,
        ..prog("x", CLAUDE, &[])
    };
    assert!(directory_rule(&truncated).is_none());
}

#[test]
fn every_regex_metacharacter_in_the_folder_is_matched_literally() {
    let hostile = [
        "/home/u/a.b+c*d?e",
        "/home/u/(x)|[y]{2}^$",
        "/home/u/a\\b",
        "/home/u/a#b&c-d~e",
        "/home/u/with space",
        "/home/u/ünï/中文",
        "/home/u/%s/\"quote\"/'q'",
        "/home/u/.*",
        "/home/u/[^/]+",
    ];
    for dir in hostile {
        let path = format!("{dir}/app/versions/1.2.3");
        let rule = rule_of(&path).unwrap_or_else(|| panic!("no rule for {dir}"));
        assert_eq!(rule.folder, format!("{dir}/app/versions/"));
        assert!(matches(&path, &path), "{dir}: its own file");
        assert!(matches(&path, &format!("{dir}/app/versions/9.9")), "{dir}");
        assert!(
            !matches(&path, &format!("{dir}/app/versions/1.2.3/x")),
            "{dir}"
        );
        assert!(
            !matches(&path, &format!("{dir}x/app/versions/1.2.3")),
            "{dir}"
        );
        // The folder is not read as a pattern: `.*` and `[^/]+` match only themselves.
        assert!(
            !matches(&path, "/home/u/anything/app/versions/1.2.3"),
            "{dir}"
        );
    }
}

#[test]
fn a_dot_in_the_app_name_is_a_dot() {
    // `<app>` is read from the path too, and may hold a dot.
    let path = "/opt/my.app/versions/1.2.3";
    assert_eq!(
        rule_of(path).unwrap().pattern,
        r"^/opt/my\.app/versions/[^/]*[^/)]$"
    );
    assert!(matches(path, "/opt/my.app/versions/2.0.0"));
    assert!(!matches(path, "/opt/myXapp/versions/1.2.3"));
}

#[test]
fn a_very_long_folder_still_makes_a_valid_rule() {
    let dir = format!("/home/u/{}", "d".repeat(900));
    let path = format!("{dir}/app/versions/1.2.3");
    assert!(rule_of(&path).is_some());
    assert!(matches(&path, &path));
}

#[test]
fn text_that_could_mislead_when_shown_gets_no_directory_rule() {
    for dir in [
        "/home/u/a\u{202E}b",
        "/home/u/a\u{200B}b",
        "/home/u/a\u{FEFF}b",
        "/home/u/a\tb",
        "/home/u/a\u{2028}b",
    ] {
        let path = format!("{dir}/app/versions/1.2.3");
        let rule = rule_of(&path);
        assert!(rule.is_none(), "{dir:?}");
    }
    // A path with a control character is not even exact: nothing at all.
    let p = one(prog(
        "x",
        "/home/u/a\nb/app/versions/1.2.3",
        &[("a.example", 2)],
    ));
    assert_eq!(
        p.choices,
        vec![Choice::NonSelectable(NonSelectable::Truncated)]
    );
}

#[test]
fn the_generated_rule_passes_the_bridges_own_policy() {
    for path in [
        CLAUDE,
        "/opt/tool/versions/1.2.3",
        "/home/u/a.b+c(d)[e]{f}/app/versions/1.2",
        "/home/u/ünï/中文/app/versions/1.2",
    ] {
        let wire = payload_of(path).to_wire();
        for profile in [PolicyProfile::Editor, PolicyProfile::Import] {
            let checked = check_wire_rule(&wire, profile);
            assert!(checked.is_ok(), "{path} as {profile:?}: {checked:?}");
        }
    }
}

#[test]
fn the_directory_rule_is_offered_beside_the_other_choices_and_never_selected() {
    let p = one(prog("2.1.308", CLAUDE, &[("a.example", 2)]));
    let kinds: Vec<ChoiceKind> = p.choices.iter().map(Choice::kind).collect();
    assert_eq!(
        kinds,
        [
            ChoiceKind::AllowHosts,
            ChoiceKind::AllowAny,
            ChoiceKind::Block,
            ChoiceKind::DirectoryRule
        ]
    );
    assert!(!p.selected_by_suggest);
}
