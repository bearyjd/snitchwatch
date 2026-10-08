use super::*;
use crate::curated::entries;

fn dir() -> tempfile::TempDir {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn flatpak_rule() -> Rule {
    entries()
        .iter()
        .find(|e| e.id == "flatpak-flathub")
        .unwrap()
        .rule()
}

fn chosen() -> Choices {
    Choices::default()
        .enable("flatpak-flathub")
        .enable("chronyc-local")
        .installed("flatpak-flathub", &flatpak_rule())
        .enable("networkmanager-connectivity-check")
}

fn write(path: &Path, text: &str, mode: u32) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
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

/// M1: only a change from off to on forgets a removal.
#[test]
fn only_turning_an_entry_on_from_off_forgets_its_removal() {
    let removed = Choices::default()
        .enable("flatpak-flathub")
        .user_removed("flatpak-flathub");
    let still = removed.enable("flatpak-flathub");
    assert!(
        still.deleted_by_user.contains("flatpak-flathub"),
        "already on"
    );
    let again = removed.disable("flatpak-flathub").enable("flatpak-flathub");
    assert!(again.enabled.contains("flatpak-flathub"));
    assert!(again.deleted_by_user.is_empty());
}

/// H1: a file that can't be trusted as a whole is an error, never "empty".
#[test]
fn a_file_that_fails_a_check_is_refused() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    for bad in [
        "not json",
        r#"{"version": 3, "choices": {}}"#,
        r#"{"version": 2, "choices": {"surprise": true}}"#,
        r#"{"version": 2, "choices": {}, "extra": 1}"#,
        r#"{"choices": {}}"#,
    ] {
        write(&path, bad, 0o600);
        assert!(load(&path).is_err(), "{bad}");
    }
    // Writable by others, or a link in place of the file.
    save(&path, &chosen()).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o666)).unwrap();
    assert!(load(&path).is_err(), "mode 0666");
    std::fs::remove_file(&path).unwrap();
    let target = dir.path().join("elsewhere.json");
    save(&target, &chosen()).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(load(&path).is_err());
}

/// Inside a readable file, what isn't valid is dropped and the rest kept;
/// a recorded copy must be the entry's exact rule (security review L2).
#[test]
fn invalid_ids_and_copies_are_dropped_and_the_rest_kept() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    let copy = |rule: &Rule| serde_json::to_value(canonical(rule)).unwrap();
    let mut other_port = flatpak_rule();
    other_port.operator.as_mut().unwrap().list[2].data = "8443".into();
    let mut precedence = flatpak_rule();
    precedence.precedence = true;
    let retired = Rule {
        name: "snitchwatch-default-retired".into(),
        ..flatpak_rule()
    };
    let file = serde_json::json!({
        "version": 2,
        "choices": {
            "enabled": ["flatpak-flathub", "Bad Id"],
            "deletedByUser": ["chronyc-local", "../x"],
            "installed": {
                "flatpak-flathub": copy(&other_port),
                "chronyc-local": copy(&precedence),
                "retired": copy(&retired),
                "networkmanager-connectivity-check": copy(&flatpak_rule()),
            }
        }
    });
    write(&path, &file.to_string(), 0o600);
    let loaded = load(&path).unwrap().unwrap();
    assert_eq!(
        loaded.enabled.iter().collect::<Vec<_>>(),
        ["flatpak-flathub"]
    );
    assert_eq!(
        loaded.deleted_by_user.iter().collect::<Vec<_>>(),
        ["chronyc-local"]
    );
    assert_eq!(
        loaded.installed.keys().collect::<Vec<_>>(),
        ["retired"],
        "only a retired curated copy survives"
    );
    // The bridge never writes what load would drop.
    let mut crafted = Choices::default();
    crafted
        .installed
        .insert("flatpak-flathub".into(), canonical(&other_port));
    assert!(save(&path, &crafted).is_err());
}

/// The version-1 file this branch wrote first is still read.
#[test]
fn a_version_1_file_is_read() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    let file = serde_json::json!({
        "version": 1,
        "choices": {
            "enabled": ["flatpak-flathub"],
            "installed": {
                "flatpak-flathub": {
                    "name": "snitchwatch-default-flatpak-flathub",
                    "rule": crate::rule_wire::rule_to_wire(&flatpak_rule()),
                }
            }
        }
    });
    write(&path, &file.to_string(), 0o600);
    let loaded = load(&path).unwrap().unwrap();
    assert_eq!(
        loaded,
        Choices::default()
            .enable("flatpak-flathub")
            .installed("flatpak-flathub", &flatpak_rule())
    );
}
