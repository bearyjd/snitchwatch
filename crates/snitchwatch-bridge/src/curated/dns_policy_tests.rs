//! The DNS entry under the curated layers around the rule (owner decision
//! S6): off by default, reconcile, the user's edits and deletes, the reserved
//! name, and what the GUI is told. The rule's own shape is in `dns_tests`.

use std::collections::{BTreeMap, BTreeSet};

use snitchwatch_proto::protocol::Rule;

use super::canonical::{canonical, is_unedited};
use super::dns_tests::{dns, ID, NAME};
use super::reconcile::{plan, CuratedAction, DaemonRules, EntryStatus};
use super::store::Choices;
use super::*;
use crate::rule_policy::{self, PolicyProfile};

/// opensnitchd's report of `rule` after a round trip: its own `created`, the
/// list's operand `list` and its JSON left in `data`.
fn as_reported(rule: &Rule) -> Rule {
    let mut reported = rule.clone();
    reported.created = 1_700_000_000;
    let op = reported.operator.as_mut().unwrap();
    op.operand = "list".into();
    op.data = r#"[{"type":"simple"}]"#.into();
    reported
}

fn daemon(rules: &[Rule]) -> BTreeMap<String, Rule> {
    rules
        .iter()
        .map(|rule| (rule.name.clone(), rule.clone()))
        .collect()
}

fn reconcile(rules: &BTreeMap<String, Rule>, choices: &Choices) -> super::reconcile::Plan {
    let none = BTreeSet::new();
    plan(
        entries(),
        DaemonRules {
            rules,
            left_out: &none,
            files_left: &none,
            maybe_applied: &none,
        },
        choices,
    )
}

#[test]
fn it_is_off_by_default_and_turned_on_installs_exactly_its_rule() {
    let nothing = reconcile(&BTreeMap::new(), &Choices::default());
    assert!(nothing.actions.is_empty(), "{:?}", nothing.actions);
    assert_eq!(nothing.statuses[ID], EntryStatus::Off);
    assert_eq!(nothing.choices, Choices::default());

    let on = Choices::default().enable(ID);
    let first = reconcile(&BTreeMap::new(), &on);
    assert_eq!(first.actions, [CuratedAction::Install(ID.into())]);
    assert_eq!(first.statuses[ID], EntryStatus::Installing);

    // Another entry turned on doesn't install this one.
    let flatpak_only = Choices::default().enable("flatpak-flathub");
    let plan = reconcile(&BTreeMap::new(), &flatpak_only);
    assert_eq!(
        plan.actions,
        [CuratedAction::Install("flatpak-flathub".into())]
    );
    assert_eq!(plan.statuses[ID], EntryStatus::Off);

    let confirmed = on.installed(ID, &dns().rule());
    let after = reconcile(&daemon(&[as_reported(&dns().rule())]), &confirmed);
    assert!(after.actions.is_empty(), "{:?}", after.actions);
    assert_eq!(after.statuses[ID], EntryStatus::Installed);
}

#[test]
fn a_first_run_with_the_rule_already_in_the_firewall_changes_nothing() {
    let rules = daemon(&[as_reported(&dns().rule())]);
    let plan = reconcile(&rules, &Choices::default());
    assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    assert_eq!(plan.statuses[ID], EntryStatus::InFirewall);
}

#[test]
fn a_user_delete_is_respected_and_an_edit_is_left_alone() {
    let installed = Choices::default().enable(ID).installed(ID, &dns().rule());
    // Deleted outside Snitchwatch: never reinstalled, across restarts too.
    let gone = reconcile(&BTreeMap::new(), &installed);
    assert!(gone.actions.is_empty(), "{:?}", gone.actions);
    assert_eq!(gone.statuses[ID], EntryStatus::DeletedOutside);
    let again = reconcile(&BTreeMap::new(), &gone.choices);
    assert!(again.actions.is_empty());
    assert_eq!(again.statuses[ID], EntryStatus::DeletedOutside);

    // Edited, in each place it could be widened: left alone, never
    // overwritten, never deleted, even on opt-out. A copy that lost its
    // sender pin, or names another user, is an edit too.
    let edits: [fn(&mut Rule); 8] = [
        |r| r.operator.as_mut().unwrap().list[2].data = "5353".into(),
        |r| r.operator.as_mut().unwrap().list[0].data = "/usr/bin/curl".into(),
        |r| r.operator.as_mut().unwrap().list[3].data = "^.*$".into(),
        |r| r.operator.as_mut().unwrap().list[1].data = "1000".into(),
        |r| {
            r.operator.as_mut().unwrap().list.remove(1);
        },
        |r| {
            r.operator.as_mut().unwrap().list.remove(3);
        },
        |r| r.precedence = true,
        |r| {
            let list = &mut r.operator.as_mut().unwrap().list;
            list[1].operand = "user.name".into();
            list[1].data = "systemd-resolve".into();
        },
    ];
    for edit in edits {
        let mut edited = dns().rule();
        edit(&mut edited);
        let rules = daemon(&[edited.clone()]);
        let on = reconcile(&rules, &installed);
        assert!(on.actions.is_empty(), "{edited:?}");
        assert_eq!(on.statuses[ID], EntryStatus::EditedByYou);
        let off = reconcile(&rules, &installed.disable(ID));
        assert!(off.actions.is_empty(), "{edited:?}");
        assert_eq!(off.statuses[ID], EntryStatus::EditedByYou);
        assert!(!is_unedited(Some(dns()), None, &edited));
    }

    // Off and unedited (a pure toggle isn't an edit): deleted.
    let toggled = Rule {
        enabled: false,
        ..as_reported(&dns().rule())
    };
    let off = reconcile(&daemon(&[toggled]), &installed.disable(ID));
    assert_eq!(
        off.actions,
        [CuratedAction::Delete {
            id: ID.into(),
            name: NAME.into()
        }]
    );
}

#[test]
fn the_daemons_report_of_the_rule_is_unedited() {
    // `user.id` is not rewritten by the daemon's Compile, so what it reports
    // is what was sent, apart from the list's operand and `created`.
    let reported = as_reported(&dns().rule());
    assert!(is_unedited(Some(dns()), None, &reported));
    assert_eq!(canonical(&reported), canonical(&dns().rule()));
    assert!(toggleable(&reported));
    let off = Rule {
        enabled: false,
        ..reported
    };
    assert!(toggleable(&off), "a toggle isn't an edit");
}

#[test]
fn the_reserved_name_keeps_users_from_adding_widening_or_deleting_it() {
    let rule = dns().rule();
    // A GUI can't add or import a rule under the name, even the shipped one.
    let problems = rule_policy::validate_user_rule(&rule, PolicyProfile::Editor).unwrap_err();
    assert!(problems.iter().any(|p| p.path == "name"), "{problems:?}");
    // Listed read-only with the shipped-default reason; not deletable from
    // the Rules page; only the unedited copy can be toggled.
    assert_eq!(
        rule_policy::read_only_reason(&rule),
        Some(rule_policy::CURATED_DEFAULT_REASON)
    );
    assert!(rule_policy::toggleable(&rule));
    assert!(!rule_policy::deletable(&rule));
    let mut edited = rule.clone();
    edited.operator.as_mut().unwrap().list[2].data = "5353".into();
    assert_eq!(
        rule_policy::read_only_reason(&edited),
        Some(rule_policy::CURATED_MANAGED_REASON)
    );
    assert!(!rule_policy::toggleable(&edited));
    assert!(!rule_policy::deletable(&edited));
    // Losing the sender pin is an edit the Rules page can't toggle either.
    let mut unpinned = rule.clone();
    unpinned.operator.as_mut().unwrap().list.remove(1);
    assert!(!rule_policy::toggleable(&unpinned));
}

/// The entry's `broad` flag is what the GUIs read ("Turn all on" skips it):
/// the any-address entry alone. The summary the bridge sends carries it
/// (checked end to end in `bridge-cli/tests/curated_dns.rs`).
#[test]
fn only_the_any_address_entry_is_broad() {
    assert!(dns().broad());
    for entry in entries().iter().filter(|entry| entry.id != ID) {
        assert!(!entry.broad(), "{}", entry.id);
    }
}
