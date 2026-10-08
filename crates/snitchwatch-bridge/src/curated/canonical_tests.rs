use super::*;
use crate::curated::entries;

fn flatpak() -> &'static CuratedEntry {
    entries()
        .iter()
        .find(|entry| entry.id == "flatpak-flathub")
        .unwrap()
}

/// What opensnitchd v1.8.0 reports after `Deserialize` → `Compile` →
/// save → load → `Serialize`: its own `created`, the list's operand set to
/// `list` and its JSON left in `data`, case-insensitive regexps lowercased.
fn as_reported(rule: &Rule) -> Rule {
    let mut reported = rule.clone();
    reported.created = 1_700_000_000;
    let op = reported.operator.as_mut().unwrap();
    op.operand = "list".into();
    op.data = r#"[{"type":"simple"}]"#.into();
    for leaf in &mut op.list {
        if leaf.r#type == "regexp" && !leaf.sensitive {
            leaf.data = leaf.data.to_lowercase();
        }
    }
    reported
}

#[test]
fn the_daemons_report_of_an_entry_is_unedited() {
    for entry in entries() {
        let reported = as_reported(&entry.rule());
        assert!(is_unedited(Some(entry), None, &reported), "{}", entry.id);
        let off = Rule {
            enabled: false,
            ..reported
        };
        assert!(
            is_unedited(Some(entry), None, &off),
            "a toggle isn't an edit"
        );
    }
}

#[test]
fn any_change_of_meaning_is_an_edit() {
    let ours = flatpak().rule();
    let edits: [fn(&mut Rule); 9] = [
        |r| r.action = "deny".into(),
        |r| r.duration = "until restart".into(),
        |r| r.precedence = true,
        |r| r.nolog = true,
        |r| r.description = "mine".into(),
        |r| r.name = "snitchwatch-default-other".into(),
        |r| r.operator.as_mut().unwrap().list[0].sensitive = false,
        |r| r.operator.as_mut().unwrap().list[2].data = "8443".into(),
        |r| r.operator.as_mut().unwrap().list.reverse(),
    ];
    for edit in edits {
        let mut edited = ours.clone();
        edit(&mut edited);
        assert!(!is_unedited(Some(flatpak()), None, &edited), "{edited:?}");
    }
    // A case-sensitive regexp keeps its case.
    let mut sensitive = ours.clone();
    let leaf = &mut sensitive.operator.as_mut().unwrap().list[3];
    leaf.sensitive = true;
    leaf.data = "^TCP6?$".into();
    let mut reported = sensitive.clone();
    reported.operator.as_mut().unwrap().list[3].data = "^tcp6?$".into();
    assert_ne!(canonical(&sensitive), canonical(&reported));
}

#[test]
fn a_retired_entry_is_compared_with_its_recorded_copy_only() {
    let retired = Rule {
        name: "snitchwatch-default-retired".into(),
        ..flatpak().rule()
    };
    let recorded = canonical(&retired);
    assert!(is_unedited(None, Some(&recorded), &as_reported(&retired)));
    assert!(
        !is_unedited(None, None, &retired),
        "nothing to compare with"
    );
    let mut wider = retired.clone();
    wider.operator.as_mut().unwrap().list.remove(1);
    assert!(!is_unedited(None, Some(&recorded), &wider));
}

#[test]
fn the_canonical_form_round_trips_to_a_curated_rule() {
    for entry in entries() {
        let form = canonical(&entry.rule());
        assert_eq!(canonical(&form.to_rule()), form);
        crate::curated::check_curated_rule(&form.to_rule()).unwrap();
        let json = serde_json::to_value(&form).unwrap();
        assert_eq!(serde_json::from_value::<CanonicalRule>(json).unwrap(), form);
    }
}

/// Info 1 of the security review: the daemon's `Compile` lowercases a
/// case-insensitive regexp in place, so every regexp the data file builds
/// must already be lowercase, or the daemon's copy would read as edited.
#[test]
fn every_regexp_an_entry_builds_is_already_lowercase() {
    for entry in entries() {
        for leaf in &entry.rule().operator.unwrap().list {
            if leaf.r#type == "regexp" {
                assert_eq!(leaf.data, leaf.data.to_lowercase(), "{}", entry.id);
            }
        }
    }
}
