//! Tests for [`super::io_view`]: what the preview sheet shows (the bridge's
//! ticks and cautions, the rule a replace overwrites), the export and
//! result texts, and the batched progress.

use super::io::MAX_IMPORT_FILE_BYTES;
use super::io_view::*;
use snitchwatch_bridge::rule_io::{
    ImportItem, ImportKind, ImportOutcome, OmittedCounts, PreviousRule,
};
use snitchwatch_bridge::ws_messages::ServerMessage;

fn item(name: &str, kind: ImportKind) -> ImportItem {
    let change = matches!(kind, ImportKind::Add | ImportKind::Replace);
    ImportItem {
        index: 0,
        name: name.into(),
        display_name: name.into(),
        kind,
        changed_fields: Vec::new(),
        problems: Vec::new(),
        weakens: false,
        applies_to_all_apps: false,
        precedence: false,
        persists: true,
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        description: String::new(),
        nolog: false,
        conditions: vec!["process.path is /usr/bin/curl".into()],
        ticked: change,
        cautions: Vec::new(),
        previous: None,
    }
}

/// The bridge decides the ticks and writes the cautions (review H2); the
/// sheet shows them as they are.
#[test]
fn rows_use_the_bridges_ticks_and_cautions() {
    let cautioned = ImportItem {
        ticked: false,
        cautions: vec![
            "This turns a blocking rule into an allow.".into(),
            "This changes what a blocking rule matches.".into(),
        ],
        ..item("swap", ImportKind::Replace)
    };
    let view = group(&[item("plain", ImportKind::Add), cautioned]);
    assert!(view.add[0].ticked && view.add[0].caution.is_empty());
    assert!(!view.replace[0].ticked);
    assert_eq!(
        view.replace[0].caution,
        "This turns a blocking rule into an allow. This changes what a blocking rule matches."
    );
}

#[test]
fn a_replace_shows_the_rule_it_overwrites() {
    let replace = ImportItem {
        changed_fields: vec!["action".into(), "logging".into()],
        previous: Some(PreviousRule {
            enabled: false,
            action: "deny".into(),
            duration: "until restart".into(),
            description: "old note".into(),
            precedence: false,
            nolog: true,
            conditions: vec!["dest.host is example.com".into()],
        }),
        ..item("swap", ImportKind::Replace)
    };
    let row = &group(&[replace]).replace[0];
    assert_eq!(
        row.previous,
        vec![
            "Action: deny (off)",
            "Lasts: until the firewall restarts",
            "Condition: dest.host is example.com",
            "Description: old note",
            "Matches aren't logged",
        ]
    );
    assert!(row
        .details
        .contains(&"Changes: action, logging".to_string()));
    assert!(group(&[item("add", ImportKind::Add)]).add[0]
        .previous
        .is_empty());
}

#[test]
fn badges_say_what_the_flags_mean() {
    let flagged = ImportItem {
        applies_to_all_apps: true,
        precedence: true,
        persists: false,
        ..item("x", ImportKind::Add)
    };
    assert_eq!(
        badges(&flagged),
        vec![
            "Applies to all apps",
            "Overrides other rules",
            "Lost when the firewall restarts"
        ]
    );
    assert!(badges(&item("plain", ImportKind::Add)).is_empty());
}

#[test]
fn the_preview_groups_rows_by_kind_in_file_order() {
    let mut refused = item("bad", ImportKind::Refused);
    refused.problems = vec![snitchwatch_bridge::rule_policy::RuleProblem {
        path: "name".into(),
        reason: "a reason".into(),
    }];
    let items = vec![
        item("b-add", ImportKind::Add),
        item("same", ImportKind::Unchanged),
        refused,
        item("a-add", ImportKind::Add),
        item("swap", ImportKind::Replace),
    ];
    let view = group(&items);
    let names = |rows: &[PreviewRow]| rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&view.add), vec!["b-add", "a-add"]);
    assert_eq!(names(&view.replace), vec!["swap"]);
    assert_eq!(names(&view.unchanged), vec!["same"]);
    assert_eq!(names(&view.refused), vec!["bad"]);
    assert_eq!(view.refused[0].problems, vec!["a reason (name)"]);
    assert!(view.add[0]
        .details
        .iter()
        .any(|d| d.contains("/usr/bin/curl")));
    assert!(view.add[0].ticked && !view.unchanged[0].ticked);
}

#[test]
fn outcomes_and_results_are_plain_sentences() {
    assert_eq!(outcome_text(&ImportOutcome::Applied), "Applied");
    assert!(outcome_text(&ImportOutcome::Rejected {
        reason: "bad regexp".into()
    })
    .contains("bad regexp"));
    assert_eq!(
        result_summary(2, 1, 0, 0),
        "2 applied, 1 refused by the firewall or Snitchwatch."
    );
}

/// Review #6: "ready to save" before the dialog, the left-out counts kept
/// through a cancel or a failed write.
#[test]
fn export_texts_say_what_happened() {
    let omitted = OmittedCounts {
        once: 1,
        managed: 2,
        ..Default::default()
    };
    let ready = export_ready(3, &omitted, 10);
    assert!(ready.starts_with("Ready to save 3 rules."), "{ready}");
    assert!(ready.contains("3 rules weren't exported"), "{ready}");
    let saved = export_saved(3, &omitted);
    assert!(saved.starts_with("Saved 3 rules."), "{saved}");
    assert!(saved.contains("3 rules weren't exported"), "{saved}");
    assert_eq!(export_cancelled(), "Nothing was saved.");
    let failed = export_failed("The file couldn't be saved there.", &omitted);
    assert!(
        failed.starts_with("The file couldn't be saved there.")
            && failed.contains("weren't exported"),
        "{failed}"
    );
    let large = export_ready(1, &OmittedCounts::default(), MAX_IMPORT_FILE_BYTES + 1);
    assert!(large.starts_with("Ready to save 1 rule."), "{large}");
    assert!(large.contains("too large to import back"), "{large}");
    assert!(large.contains("2,000 to 3,500 rules"), "{large}");
}

/// The export is compact JSON: it holds about twice the rules pretty JSON
/// would within the import cap (review #17).
#[test]
fn the_export_file_is_compact_json() {
    let document = snitchwatch_bridge::rule_io::Document {
        format: "snitchwatch.rules".into(),
        version: 1,
        exported_at_unix_ms: 1,
        source: Default::default(),
        rules: vec![serde_json::json!({ "name": "a" })],
    };
    let text = export_text(&document);
    assert!(!text.contains('\n') && !text.contains(": "), "{text}");
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
}

/// Review #9: outcomes are collected as they arrive and handed to QML in
/// one batch, not re-serialized per rule.
#[test]
fn progress_is_flushed_in_batches() {
    let mut log = ProgressLog::default();
    assert_eq!(log.take_json(), None);
    log.record("a".into(), "Applied".into());
    log.record("b".into(), "Applied".into());
    let json = log.take_json().expect("dirty");
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["a"], "Applied");
    assert_eq!(parsed["b"], "Applied");
    assert_eq!(log.take_json(), None, "nothing new");
    log.record("c".into(), "Applied".into());
    let json = log.take_json().unwrap();
    assert!(json.contains("\"a\"") && json.contains("\"c\""), "{json}");
    log.clear();
    assert_eq!(log.take_json().as_deref(), Some("{}"));
}

#[test]
fn the_controller_listens_only_to_import_and_export_messages() {
    assert!(interests_rules_io(&ServerMessage::RulesImportRefused {
        request_id: String::new(),
        reason: String::new()
    }));
    assert!(interests_rules_io(&ServerMessage::RulesImportResult {
        preview_id: String::new(),
        applied: 0,
        rejected: 0,
        not_sent: 0,
        no_answer: 0,
    }));
    assert!(!interests_rules_io(&ServerMessage::SetRules {
        rules: Vec::new()
    }));
}

/// Problem paths are field names; the sheet shows plain words (review H2's
/// "no raw field names in user text").
#[test]
fn problem_locations_are_plain_words() {
    let problem = |path: &str| snitchwatch_bridge::rule_policy::RuleProblem {
        path: path.into(),
        reason: "a reason".into(),
    };
    let mut refused = item("bad", ImportKind::Refused);
    refused.problems = vec![
        problem("operator.list[1].data"),
        problem("operator.list[0].operand"),
        problem("operator.data"),
        problem("enabled"),
        problem("duration"),
        problem("operator"),
        problem("rule"),
    ];
    assert_eq!(
        group(&[refused]).refused[0].problems,
        vec![
            "a reason (condition 2's value)",
            "a reason (condition 1)",
            "a reason (the condition's value)",
            "a reason (on or off)",
            "a reason (how long it lasts)",
            "a reason (conditions)",
            "a reason",
        ]
    );
}

/// Review #8: the controller acts only on the answer it waits for.
#[test]
fn only_the_awaited_answer_is_taken() {
    let request = Waiting::Request("r1".into());
    let export = |id: &str| ServerMessage::RulesExportUnavailable {
        request_id: id.into(),
        reason: String::new(),
    };
    assert!(awaits(&request, &export("r1")));
    assert!(!awaits(&request, &export("r2")));
    assert!(!awaits(&Waiting::Nothing, &export("r1")));

    let apply = Waiting::Apply {
        preview_id: "p1".into(),
        request_id: "a1".into(),
    };
    let progress = |id: &str| ServerMessage::RulesImportProgress {
        preview_id: id.into(),
        name: "x".into(),
        outcome: ImportOutcome::Applied,
    };
    let result = |id: &str| ServerMessage::RulesImportResult {
        preview_id: id.into(),
        applied: 0,
        rejected: 0,
        not_sent: 0,
        no_answer: 0,
    };
    let refused = |id: &str| ServerMessage::RulesImportRefused {
        request_id: id.into(),
        reason: String::new(),
    };
    assert!(awaits(&apply, &progress("p1")) && awaits(&apply, &result("p1")));
    assert!(!awaits(&apply, &progress("p2")) && !awaits(&apply, &result("p2")));
    assert!(awaits(&apply, &refused("a1")));
    assert!(!awaits(&apply, &refused("a2")));
    assert!(
        !awaits(&request, &progress("r1")),
        "a preview isn't an apply"
    );
}
