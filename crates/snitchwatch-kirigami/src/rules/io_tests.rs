//! Tests for [`super::io`]: the import file read and the export file write.

use super::io::*;
use snitchwatch_bridge::ws_messages::ClientMessage;
use std::os::unix::fs::PermissionsExt;

const DOC: &str = r#"{"format":"snitchwatch.rules","version":1,"rules":[]}"#;

// --- Reading an import file ------------------------------------------------

#[test]
fn a_document_is_read_and_turned_into_a_preview_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.json");
    std::fs::write(&path, DOC).unwrap();
    let document = read_import_file(&path).unwrap();
    match preview_request("r".into(), document).unwrap() {
        ClientMessage::PreviewRulesImport { document, .. } => {
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
    assert_eq!(
        preview_request("r".into(), document),
        Err(FileError::TooLarge)
    );
}

#[test]
fn errors_are_fixed_text() {
    // The over-cap refusal says how much fits, in rules, not only bytes.
    let too_large = FileError::TooLarge.describe();
    assert!(
        too_large.contains("960 KiB") && too_large.contains("2,000 to 3,500 rules"),
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
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Review L2: the export is written to a new temporary file and renamed
/// into place, so a symlink at the chosen path is replaced, never followed.
#[test]
fn an_export_never_follows_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::write(&elsewhere, "keep me").unwrap();
    let link = dir.path().join("rules.json");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

    write_export_file(&link, DOC).unwrap();

    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "keep me");
    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(
        meta.file_type().is_file(),
        "the link was replaced by a file"
    );
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read_to_string(&link).unwrap(), DOC);
}
