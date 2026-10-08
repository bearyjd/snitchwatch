use super::*;
use crate::curated::entries;

fn dir() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn chosen() -> Choices {
    let flatpak = entries()
        .iter()
        .find(|e| e.id == "flatpak-flathub")
        .unwrap();
    Choices::default()
        .enable("flatpak-flathub")
        .enable("chronyc-local")
        .installed("flatpak-flathub", &flatpak.rule())
        .enable("systemd-resolved-dns")
}

#[test]
fn choices_come_back_unchanged_and_nothing_is_on_without_a_file() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    assert_eq!(load(&path).unwrap(), None);
    let mut deleted = chosen();
    deleted
        .deleted_by_user
        .insert("networkmanager-connectivity-check".into());
    save(&path, &deleted).unwrap();
    assert_eq!(load(&path).unwrap(), Some(deleted));
    assert!(
        Choices::default().enabled.is_empty(),
        "opt-in: off by default"
    );
}

#[test]
fn turning_an_entry_on_again_forgets_its_deletion() {
    let mut deleted = Choices::default();
    deleted.deleted_by_user.insert("flatpak-flathub".into());
    let again = deleted.enable("flatpak-flathub");
    assert!(again.enabled.contains("flatpak-flathub"));
    assert!(again.deleted_by_user.is_empty());
    assert!(!again
        .disable("flatpak-flathub")
        .enabled
        .contains("flatpak-flathub"));
}

#[test]
fn a_file_that_fails_a_check_is_refused() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    for bad in [
        "not json".to_string(),
        r#"{"version": 2, "choices": {}}"#.to_string(),
        r#"{"version": 1, "choices": {"enabled": ["Bad Id"]}}"#.to_string(),
        r#"{"version": 1, "choices": {"surprise": true}}"#.to_string(),
        // An installed copy that isn't the curated shape (here: precedence).
        format!(
            r#"{{"version": 1, "choices": {{"installed": {{"flatpak-flathub": {{"name": "snitchwatch-default-flatpak-flathub", "rule": {}}}}}}}}}"#,
            {
                let flatpak = entries()
                    .iter()
                    .find(|e| e.id == "flatpak-flathub")
                    .unwrap();
                let mut wire = crate::rule_wire::rule_to_wire(&flatpak.rule());
                wire["precedence"] = serde_json::json!(true);
                wire
            }
        ),
    ] {
        std::fs::write(&path, &bad).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        assert!(load(&path).is_err(), "{bad}");
    }
    // A link in place of the file is not followed.
    std::fs::remove_file(&path).unwrap();
    let target = dir.path().join("elsewhere.json");
    save(&target, &chosen()).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(load(&path).is_err());
}
