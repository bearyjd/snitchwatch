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
        r#"{"version": 4, "choices": {}}"#,
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
    // Re-review M2: the ids of dropped copies are kept.
    assert_eq!(
        loaded.installed_ids.iter().collect::<Vec<_>>(),
        [
            "chronyc-local",
            "flatpak-flathub",
            "networkmanager-connectivity-check",
            "retired"
        ]
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

const DNS: &str = "dns-resolved";

fn granted() -> Choices {
    Choices::default().enable(DNS).grant_broad(DNS, true)
}

fn missing_plan(choices: &Choices) -> crate::curated::reconcile::Plan {
    let none = BTreeSet::new();
    crate::curated::reconcile::plan(
        entries(),
        crate::curated::reconcile::DaemonRules {
            rules: &BTreeMap::new(),
            left_out: &none,
            files_left: &none,
            maybe_applied: &none,
        },
        choices,
    )
}

#[test]
fn broad_authority_is_durable_and_off_or_removal_revokes_it() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    let choices = granted();
    save(&path, &choices).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap()
            ["version"],
        3
    );
    let restarted = load(&path).unwrap().unwrap();
    assert_eq!(restarted, choices);
    assert!(restarted.has_broad_authority(DNS));
    assert_eq!(
        missing_plan(&restarted).actions,
        [crate::curated::reconcile::CuratedAction::Install(
            DNS.into()
        )]
    );
    for revoked in [
        restarted.disable(DNS),
        restarted.user_removed(DNS),
        restarted.revoke_broad(DNS),
    ] {
        assert!(!revoked.has_broad_authority(DNS));
        assert!(revoked.broad_grants.is_empty());
        save(&path, &revoked).unwrap();
        assert!(missing_plan(&load(&path).unwrap().unwrap())
            .actions
            .is_empty());
        assert!(!load(&path)
            .unwrap()
            .unwrap()
            .enable(DNS)
            .has_broad_authority(DNS));
    }
    let only_revoke = choices.revoke_broad(DNS);
    assert_eq!(only_revoke.enabled, choices.enabled);
    assert_eq!(only_revoke.disabled, choices.disabled);
}

#[test]
fn legacy_enabled_choices_and_files_are_preserved_without_authority() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    for version in [1, 2] {
        let bytes =
            serde_json::json!({"version": version, "choices": {"enabled": [DNS]}}).to_string();
        write(&path, &bytes, 0o600);
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.enabled, BTreeSet::from([DNS.into()]));
        assert!(!loaded.has_broad_authority(DNS));
        let refused = missing_plan(&loaded);
        assert!(refused.actions.is_empty());
        assert_eq!(
            refused.statuses[DNS],
            crate::curated::reconcile::EntryStatus::AwaitingConsent
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
    // A v2 file cannot claim modern consent by injecting v3 metadata.
    let file = serde_json::json!({"version": 2, "choices": granted()});
    write(&path, &file.to_string(), 0o600);
    assert!(!load(&path).unwrap().unwrap().has_broad_authority(DNS));
}

#[test]
fn invalid_or_mismatched_grants_leave_choices_without_authority() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    let mut bad_grants = vec![
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!(true),
    ];
    let grant = serde_json::to_value(&granted().broad_grants[DNS]).unwrap();
    for field in ["version", "id", "consent", "policySha256", "authority"] {
        let mut missing = grant.clone();
        missing.as_object_mut().unwrap().remove(field);
        bad_grants.push(serde_json::json!({DNS: missing}));
    }
    for (field, bad) in [
        ("version", serde_json::json!(2)),
        ("id", serde_json::json!("flatpak-flathub")),
        ("consent", serde_json::json!("future-consent")),
        ("policySha256", serde_json::json!("0".repeat(64))),
        ("authority", serde_json::json!("admin")),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut altered = grant.clone();
        altered[field] = bad;
        bad_grants.push(serde_json::json!({DNS: altered}));
    }
    for metadata in bad_grants {
        let file = serde_json::json!({"version": 3, "choices": {"enabled": [DNS], "broadGrants": metadata}});
        write(&path, &file.to_string(), 0o600);
        let loaded = load(&path).unwrap().unwrap();
        assert!(loaded.enabled.contains(DNS));
        assert!(!loaded.has_broad_authority(DNS));
        assert!(loaded.broad_grants.is_empty());
        assert!(missing_plan(&loaded).actions.is_empty());
    }
    // A valid field beside a duplicate cannot launder a malformed record.
    let valid = grant.to_string();
    let duplicate_field = valid.replacen("{", "{\"version\":99,", 1);
    for grants in [
        format!("{{\"{DNS}\":{duplicate_field}}}"),
        format!("{{\"{DNS}\":{valid},\"{DNS}\":{valid}}}"),
    ] {
        write(&path, &format!("{{\"version\":3,\"choices\":{{\"enabled\":[\"{DNS}\"],\"broadGrants\":{grants}}}}}"), 0o600);
        let loaded = load(&path).unwrap().unwrap();
        assert!(loaded.enabled.contains(DNS));
        assert!(!loaded.has_broad_authority(DNS));
    }
}

#[test]
fn adoption_only_and_choice_enable_cannot_elevate_to_activation() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    let adopted = Choices::default().enable(DNS).grant_broad(DNS, false);
    save(&path, &adopted).unwrap();
    let loaded = load(&path).unwrap().unwrap().enable(DNS);
    assert_eq!(
        loaded.broad_grants[DNS].authority,
        BroadAuthority::AdoptionOnly
    );
    assert!(!loaded.has_broad_authority(DNS));
    assert!(missing_plan(&loaded).actions.is_empty());
    assert!(!Choices::default().enable(DNS).has_broad_authority(DNS));
    assert!(Choices::default()
        .grant_broad("flatpak-flathub", true)
        .broad_grants
        .is_empty());
}

#[test]
fn fingerprint_binds_every_canonical_policy_field_and_ignores_transients() {
    let entry = entries().iter().find(|entry| entry.id == DNS).unwrap();
    let fingerprint = broad_policy_fingerprint(entry);
    let mut transient = entry.rule();
    transient.enabled = false;
    transient.created = 123;
    assert_eq!(
        fingerprint,
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&canonical(&transient)).unwrap())
        )
    );
    let mut mutations: Vec<Rule> = Vec::new();
    for operand in ["process.path", "user.id", "dest.port", "protocol"] {
        let mut rule = entry.rule();
        rule.operator
            .as_mut()
            .unwrap()
            .list
            .iter_mut()
            .find(|leaf| leaf.operand == operand)
            .unwrap()
            .data
            .push('x');
        mutations.push(rule);
    }
    let mut destination = entry.rule();
    destination
        .operator
        .as_mut()
        .unwrap()
        .list
        .push(snitchwatch_proto::protocol::Operator {
            r#type: "simple".into(),
            operand: "dest.ip".into(),
            data: "127.0.0.1".into(),
            sensitive: true,
            list: vec![],
        });
    mutations.push(destination);
    for field in [
        "name",
        "action",
        "duration",
        "description",
        "precedence",
        "nolog",
    ] {
        let mut rule = entry.rule();
        match field {
            "name" => rule.name.push('x'),
            "action" => rule.action = "deny".into(),
            "duration" => rule.duration = "once".into(),
            "description" => rule.description.push('x'),
            "precedence" => rule.precedence = true,
            "nolog" => rule.nolog = !rule.nolog,
            _ => unreachable!(),
        }
        mutations.push(rule);
    }
    for rule in mutations {
        assert_ne!(
            fingerprint,
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&canonical(&rule)).unwrap())
            )
        );
    }
}

#[test]
fn broad_grants_retain_atomic_owner_only_and_no_link_guards() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    save(&path, &granted()).unwrap();
    let original_choices = std::fs::read(&path).unwrap();
    let mut invalid_grant = granted();
    invalid_grant
        .broad_grants
        .get_mut(DNS)
        .unwrap()
        .policy_sha256 = "0".repeat(64);
    assert!(save(&path, &invalid_grant).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original_choices);
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions())
            & 0o777,
        0o600
    );
    let target = dir.path().join("target.json");
    save(&target, &granted()).unwrap();
    let original = std::fs::read(&target).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(save(&path, &granted().disable(DNS)).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert!(load(&path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&target, &path).unwrap();
    assert!(save(&path, &granted()).is_err());
    assert!(load(&path).is_err());
}

/// #120 item 18: the "file may remain" bit is kept across a save and a
/// load, and written only while set, so a file without one is still read by
/// an older bridge. An id that isn't one is dropped like any other.
#[test]
fn a_file_left_bit_round_trips_and_is_written_only_when_set() {
    let dir = dir();
    let path = dir.path().join(FILE_NAME);
    let noted = chosen().file_left("flatpak-flathub");
    save(&path, &noted).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains(r#""filesLeft":["flatpak-flathub"]"#));
    assert_eq!(load(&path).unwrap(), Some(noted.clone()));
    let gone = noted.file_gone("flatpak-flathub");
    assert_eq!(gone, chosen());
    save(&path, &gone).unwrap();
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("filesLeft"));
    assert_eq!(load(&path).unwrap(), Some(gone));
    write(
        &path,
        r#"{"version":3,"choices":{"filesLeft":["flatpak-flathub","Not An Id"]}}"#,
        0o600,
    );
    assert_eq!(
        load(&path).unwrap().unwrap().files_left,
        ["flatpak-flathub".to_string()].into()
    );
}
