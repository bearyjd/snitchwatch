use super::*;
use crate::curated::entries;

fn flatpak() -> &'static CuratedEntry {
    entries()
        .iter()
        .find(|entry| entry.id == "flatpak-flathub")
        .unwrap()
}

fn daemon(rules: &[Rule]) -> BTreeMap<String, Rule> {
    rules
        .iter()
        .map(|rule| (rule.name.clone(), rule.clone()))
        .collect()
}

fn status(plan: &Plan, id: &str) -> EntryStatus {
    plan.statuses[id]
}

#[test]
fn nothing_is_on_by_default() {
    let plan = plan(entries(), &BTreeMap::new(), &Choices::default());
    assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    assert!(plan.statuses.values().all(|s| *s == EntryStatus::Off));
    assert_eq!(plan.choices, Choices::default());
}

#[test]
fn an_entry_turned_on_is_installed_and_then_reported_installed() {
    let on = Choices::default().enable("flatpak-flathub");
    let first = plan(entries(), &BTreeMap::new(), &on);
    assert_eq!(
        first.actions,
        [CuratedAction::Install("flatpak-flathub".into())]
    );
    assert_eq!(status(&first, "flatpak-flathub"), EntryStatus::Installing);

    let confirmed = on.installed("flatpak-flathub", &flatpak().rule());
    let after = plan(entries(), &daemon(&[flatpak().rule()]), &confirmed);
    assert!(after.actions.is_empty());
    assert_eq!(status(&after, "flatpak-flathub"), EntryStatus::Installed);
}

#[test]
fn a_rule_deleted_outside_snitchwatch_is_never_reinstalled() {
    let installed = Choices::default()
        .enable("flatpak-flathub")
        .installed("flatpak-flathub", &flatpak().rule());
    // The daemon's list no longer has it.
    let gone = plan(entries(), &BTreeMap::new(), &installed);
    assert!(gone.actions.is_empty(), "reinstalled: {:?}", gone.actions);
    assert_eq!(
        status(&gone, "flatpak-flathub"),
        EntryStatus::DeletedOutside
    );
    assert!(gone.choices.deleted_by_user.contains("flatpak-flathub"));

    // Across restarts: the remembered choice still holds.
    let again = plan(entries(), &BTreeMap::new(), &gone.choices);
    assert!(again.actions.is_empty());
    assert_eq!(
        status(&again, "flatpak-flathub"),
        EntryStatus::DeletedOutside
    );

    // Until the user turns it on again.
    let asked = gone
        .choices
        .disable("flatpak-flathub")
        .enable("flatpak-flathub");
    let reinstall = plan(entries(), &BTreeMap::new(), &asked);
    assert_eq!(
        reinstall.actions,
        [CuratedAction::Install("flatpak-flathub".into())]
    );
}

#[test]
fn turning_an_entry_off_deletes_only_an_unedited_copy() {
    let installed = Choices::default().installed("flatpak-flathub", &flatpak().rule());
    // Off, unedited (a pure toggle of `enabled` is not an edit).
    let toggled = Rule {
        enabled: false,
        ..flatpak().rule()
    };
    let off = plan(entries(), &daemon(&[toggled]), &installed);
    assert_eq!(
        off.actions,
        [CuratedAction::Delete {
            id: "flatpak-flathub".into(),
            name: "snitchwatch-default-flatpak-flathub".into(),
        }]
    );
    // Off, but edited: left alone, even on opt-out.
    let mut edited = flatpak().rule();
    edited.operator.as_mut().unwrap().list[2].data = "8443".into();
    let kept = plan(entries(), &daemon(&[edited.clone()]), &installed);
    assert!(kept.actions.is_empty(), "{:?}", kept.actions);
    assert_eq!(status(&kept, "flatpak-flathub"), EntryStatus::EditedByYou);
    // And on: still left alone, never reinstalled over the edit.
    let on = plan(
        entries(),
        &daemon(&[edited]),
        &installed.enable("flatpak-flathub"),
    );
    assert!(on.actions.is_empty());
    assert_eq!(status(&on, "flatpak-flathub"), EntryStatus::EditedByYou);
}

#[test]
fn a_rule_turned_off_in_the_rule_list_is_installed_but_off() {
    let installed = Choices::default()
        .enable("flatpak-flathub")
        .installed("flatpak-flathub", &flatpak().rule());
    let toggled = Rule {
        enabled: false,
        ..flatpak().rule()
    };
    let plan = plan(entries(), &daemon(&[toggled]), &installed);
    assert!(
        plan.actions.is_empty(),
        "a pure toggle is the user's choice"
    );
    assert_eq!(
        status(&plan, "flatpak-flathub"),
        EntryStatus::InstalledButOff
    );
}

#[test]
fn only_our_own_rules_are_ever_deleted() {
    let user_rule = Rule {
        name: "flatpak-my-rule".into(),
        ..flatpak().rule()
    };
    // Under the prefix but not ours (never installed, not in the file).
    let squatter = Rule {
        name: "snitchwatch-default-unknown".into(),
        ..flatpak().rule()
    };
    // Ours, but no longer in the file: deleted while unedited.
    let retired = Rule {
        name: "snitchwatch-default-retired".into(),
        ..flatpak().rule()
    };
    // A profile's rule (#46 Part 2), even one shaped like ours.
    let profile = Rule {
        name: "850-profile:home:0000-flatpak-flathub".into(),
        ..flatpak().rule()
    };
    let choices = Choices::default().installed("retired", &retired);
    let plan = plan(
        entries(),
        &daemon(&[user_rule, squatter, retired, profile]),
        &choices,
    );
    assert_eq!(
        plan.actions,
        [CuratedAction::Delete {
            id: "retired".into(),
            name: "snitchwatch-default-retired".into(),
        }]
    );
}

#[test]
fn an_unrecorded_copy_matching_the_file_is_adopted() {
    // The record was lost (e.g. a new state directory).
    let plan = plan(
        entries(),
        &daemon(&[flatpak().rule()]),
        &Choices::default().enable("flatpak-flathub"),
    );
    assert!(plan.actions.is_empty());
    assert!(plan.choices.installed.contains_key("flatpak-flathub"));
    assert_eq!(status(&plan, "flatpak-flathub"), EntryStatus::Installed);
}

#[test]
fn the_comparison_ignores_member_order_and_enabled_only() {
    let ours = flatpak().rule();
    let mut reordered = ours.clone();
    reordered.operator.as_mut().unwrap().list.reverse();
    reordered.enabled = false;
    reordered.created = 1_700_000_000;
    assert!(same_ignoring_enabled(&ours, &reordered));
    for change in [
        |r: &mut Rule| r.action = "deny".into(),
        |r: &mut Rule| r.duration = "until restart".into(),
        |r: &mut Rule| r.precedence = true,
        |r: &mut Rule| r.description = "mine".into(),
        |r: &mut Rule| r.operator.as_mut().unwrap().list[0].sensitive = false,
    ] {
        let mut edited = ours.clone();
        change(&mut edited);
        assert!(!same_ignoring_enabled(&ours, &edited));
    }
}
