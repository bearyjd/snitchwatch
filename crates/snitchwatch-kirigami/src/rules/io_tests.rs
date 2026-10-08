//! Tests for [`super::io`]: the import file read, the export file write,
//! the preview's grouping and default ticks.

use super::io::*;
use snitchwatch_bridge::rule_io::{ImportItem, ImportKind, ImportOutcome, OmittedCounts};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use std::os::unix::fs::PermissionsExt;

fn item(name: &str, kind: ImportKind) -> ImportItem {
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
    }
}

const DOC: &str = r#"{"format":"snitchwatch.rules","version":1,"rules":[]}"#;

// --- Reading an import file ------------------------------------------------

#[test]
fn a_document_is_read_and_turned_into_a_preview_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.json");
    std::fs::write(&path, DOC).unwrap();
    let document = read_import_file(&path).unwrap();
    match preview_request(document).unwrap() {
        ClientMessage::PreviewRulesImport { document } => {
            assert_eq!(document["format"], "snitchwatch.rules")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_read_cap_is_checked_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.json");
    let at_cap = format!("[{}]", " ".repeat(MAX_IMPORT_FILE_BYTES - 2));
    assert_eq!(at_cap.len(), MAX_IMPORT_FILE_BYTES);
    std::fs::write(&path, &at_cap).unwrap();
    assert!(read_import_file(&path).is_ok());

    std::fs::write(&path, format!("{at_cap} ")).unwrap();
    assert_eq!(read_import_file(&path), Err(FileError::TooLarge));
}

#[test]
fn only_regular_json_files_are_read() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_import_file(dir.path()), Err(FileError::NotAFile));

    let fifo = dir.path().join("fifo");
    let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo has no other preconditions.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert_eq!(
        read_import_file(&fifo),
        Err(FileError::NotAFile),
        "a FIFO would block the UI thread"
    );

    assert_eq!(
        read_import_file(&dir.path().join("missing.json")),
        Err(FileError::Unreadable)
    );
    let text = dir.path().join("text.json");
    std::fs::write(&text, "not json").unwrap();
    assert_eq!(read_import_file(&text), Err(FileError::NotJson));
}

#[test]
fn a_request_over_the_bridge_message_cap_is_not_sent() {
    let pad = "a".repeat(snitchwatch_bridge::ws_server::MAX_CLIENT_MESSAGE_BYTES);
    let document = serde_json::json!({ "format": "snitchwatch.rules", "pad": pad });
    assert_eq!(preview_request(document), Err(FileError::TooLarge));
}

#[test]
fn errors_are_fixed_text() {
    // The over-cap refusal says how much fits, in rules, not only bytes.
    let too_large = FileError::TooLarge.describe();
    assert!(
        too_large.contains("960 KiB") && too_large.contains("1,000 to 2,000 rules"),
        "{too_large}"
    );
    for error in [
        FileError::NotLocal,
        FileError::NotAFile,
        FileError::TooLarge,
        FileError::Unreadable,
        FileError::NotJson,
        FileError::NotWritten,
    ] {
        assert!(!error.describe().is_empty());
    }
}

// --- Writing an export file ------------------------------------------------

#[test]
fn an_export_is_written_owner_only_even_over_a_readable_file() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("fresh.json");
    write_export_file(&fresh, DOC).unwrap();
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), DOC);
    let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let existing = dir.path().join("existing.json");
    std::fs::write(&existing, "old contents that are longer than the new ones").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_export_file(&existing, DOC).unwrap();
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), DOC);
    let mode = std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    assert_eq!(
        write_export_file(dir.path(), DOC),
        Err(FileError::NotWritten)
    );
}

// --- Preview grouping and default ticks -------------------------------------

#[test]
fn adds_and_replaces_start_ticked_unless_they_loosen() {
    let add = item("add", ImportKind::Add);
    let replace = item("replace", ImportKind::Replace);
    assert!(default_ticked(&add) && default_ticked(&replace));

    let weakens = ImportItem {
        weakens: true,
        ..item("weakens", ImportKind::Replace)
    };
    let precedence_allow = ImportItem {
        precedence: true,
        action: "allow".into(),
        ..item("p", ImportKind::Add)
    };
    let all_apps_allow = ImportItem {
        applies_to_all_apps: true,
        action: "allow".into(),
        ..item("a", ImportKind::Add)
    };
    for unticked in [&weakens, &precedence_allow, &all_apps_allow] {
        assert!(!default_ticked(unticked), "{}", unticked.name);
        assert!(caution(unticked).is_some(), "{}", unticked.name);
    }

    // A deny that applies to every app, or overrides others, still starts
    // ticked: it blocks more, never less.
    let all_apps_deny = ImportItem {
        applies_to_all_apps: true,
        precedence: true,
        ..item("d", ImportKind::Add)
    };
    assert!(default_ticked(&all_apps_deny));
    assert!(caution(&all_apps_deny).is_none());

    assert!(!default_ticked(&item("u", ImportKind::Unchanged)));
    assert!(!default_ticked(&item("r", ImportKind::Refused)));
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
fn outcomes_and_summaries_are_plain_sentences() {
    assert_eq!(outcome_text(&ImportOutcome::Applied), "Applied");
    assert!(outcome_text(&ImportOutcome::Rejected {
        reason: "bad regexp".into()
    })
    .contains("bad regexp"));
    assert_eq!(
        result_summary(2, 1, 0, 0),
        "2 applied, 1 refused by the firewall or Snitchwatch."
    );
    let summary = export_summary(
        3,
        &OmittedCounts {
            once: 1,
            managed: 2,
            ..Default::default()
        },
        10,
    );
    assert!(summary.starts_with("Exported 3 rules."), "{summary}");
    assert!(summary.contains("3 rules weren't exported"), "{summary}");
    let large = export_summary(1, &OmittedCounts::default(), MAX_IMPORT_FILE_BYTES + 1);
    assert!(large.contains("too large to import back"), "{large}");
    assert!(large.contains("1,000 to 2,000 rules"), "{large}");
}

#[test]
fn the_controller_listens_only_to_import_and_export_messages() {
    assert!(interests_rules_io(&ServerMessage::RulesImportRefused {
        reason: String::new()
    }));
    assert!(interests_rules_io(&ServerMessage::RulesImportResult {
        applied: 0,
        rejected: 0,
        not_sent: 0,
        no_answer: 0,
    }));
    assert!(!interests_rules_io(&ServerMessage::SetRules {
        rules: Vec::new()
    }));
}
