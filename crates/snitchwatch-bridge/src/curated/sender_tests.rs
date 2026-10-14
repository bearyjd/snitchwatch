//! Sender pins (#120 items 36/41): the fixed-account entries are pinned
//! by user ID, the run-time accounts' by the uid their copy names, and a
//! copy installed before the pin is recognised as Snitchwatch's own.

use std::collections::{BTreeMap, BTreeSet};

use snitchwatch_proto::protocol::Rule;

use super::canonical::{is_legacy, is_unedited};
use super::reconcile::{inert_statuses, plan, CuratedAction, DaemonRules, EntryStatus};
use super::store::Choices;
use super::*;

fn entry(id: &str) -> &'static CuratedEntry {
    entries().iter().find(|entry| entry.id == id).unwrap()
}

fn operands(rule: &Rule) -> Vec<String> {
    let op = rule.operator.as_ref().unwrap();
    op.list.iter().map(|leaf| leaf.operand.clone()).collect()
}

fn uid_leaf(rule: &Rule) -> Option<String> {
    let op = rule.operator.as_ref().unwrap();
    op.list
        .iter()
        .find(|leaf| leaf.operand == "user.id")
        .map(|leaf| leaf.data.clone())
}

const ROOT_PINNED: [&str; 3] = [
    "networkmanager-connectivity-check",
    "tailscale-control",
    "tailscale-logs",
];
const ACCOUNTS: [(&str, &str); 2] = [("chronyd-ntp", "chrony"), ("avahi-mdns", "avahi")];

#[test]
fn networkmanager_and_tailscale_are_pinned_to_root() {
    for id in ROOT_PINNED {
        let rule = entry(id).rule();
        assert_eq!(
            operands(&rule),
            [
                "process.path",
                "dest.host",
                "user.id",
                "dest.port",
                "protocol"
            ],
            "{id}"
        );
        assert_eq!(uid_leaf(&rule).as_deref(), Some("0"), "{id}");
        assert!(check_curated_rule(&rule).is_ok(), "{id}");
        assert_eq!(entry(id).account(), None);
    }
}

#[test]
fn chronyd_and_avahi_are_pinned_to_their_accounts_at_run_time() {
    for (id, account) in ACCOUNTS {
        let entry = entry(id);
        assert_eq!(entry.account(), Some(account));
        let rule = entry.rule_as(Some(991));
        assert_eq!(operands(&rule)[2], "user.id", "{id}");
        assert_eq!(uid_leaf(&rule).as_deref(), Some("991"));
        assert!(check_curated_rule(&rule).is_ok(), "{id}");
        // Unresolved, the rule can never be sent.
        assert_eq!(uid_leaf(&entry.rule()).as_deref(), Some(""));
        assert!(check_curated_rule(&entry.rule()).is_err(), "{id}");
    }
    assert_eq!(account_names(), ["systemd-resolve", "chrony", "avahi"]);
}

#[test]
fn chronyc_and_flatpak_stay_path_only() {
    for id in ["chronyc-local", "flatpak-flathub"] {
        assert_eq!(entry(id).sender, None, "{id}");
        assert_eq!(uid_leaf(&entry(id).rule()), None, "{id}");
        assert!(entry(id).legacy_rule().is_none(), "{id}");
    }
}

/// The broad grant hashes the DNS rule: the pins leave it as it was, so no
/// one who consented is asked again.
#[test]
fn the_dns_rule_is_unchanged() {
    let dns = entry("dns-resolved");
    assert_eq!(
        operands(&dns.rule()),
        ["process.path", "user.id", "dest.port", "protocol"]
    );
    assert!(dns.legacy_rule().is_none());
    assert_eq!(
        store::broad_policy_fingerprint(dns),
        "28275345efbd13d2f3324a90e9e1270a000613f45fa964c5768ed631102b7f9c"
    );
}

#[test]
fn the_allowlist_takes_a_user_id_only_in_its_place_and_form() {
    let pinned = entry("networkmanager-connectivity-check").rule();
    let with = |change: &dyn Fn(&mut Vec<snitchwatch_proto::protocol::Operator>)| {
        let mut rule = pinned.clone();
        change(&mut rule.operator.as_mut().unwrap().list);
        check_curated_rule(&rule)
    };
    assert!(with(&|_| ()).is_ok());
    for bad in ["00", "-1", "01", " 0", "4294967296", "root", ""] {
        assert!(with(&|l| l[2].data = bad.into()).is_err(), "{bad:?}");
    }
    assert!(with(&|l| l[2].sensitive = true).is_err());
    assert!(with(&|l| l[2].r#type = "regexp".into()).is_err());
    assert!(with(&|l| l.swap(1, 2)).is_err(), "before the destination");
    assert!(with(&|l| {
        let again = l[2].clone();
        l.insert(2, again);
    })
    .is_err());
    // Any address still needs exactly the resolver's pin.
    let mut dns = entry("dns-resolved").rule();
    dns.operator.as_mut().unwrap().list[1].data = "0".into();
    assert!(check_curated_rule(&dns).is_err());
}

#[test]
fn a_data_file_sender_is_checked() {
    let bad = |sender: &str| {
        let raw = DATA.replacen(
            r#""id": "chronyd-ntp","#,
            &format!(r#""id": "chronyd-ntp", "sender": {sender},"#),
            1,
        );
        parse(&raw).is_err()
    };
    // `chronyd-ntp` already names its account: a second sender is refused.
    assert!(bad(r#"{"userId": 0}"#));
    let raw = DATA.replacen(r#""account": "chrony""#, r#""account": "Chrony; rm""#, 1);
    assert!(parse(&raw).is_err(), "not an account name");
    let raw = DATA.replacen(
        r#""id": "dns-resolved","#,
        r#""id": "dns-resolved", "sender": {"userId": 0},"#,
        1,
    );
    assert!(parse(&raw).is_err(), "the DNS entry's pin is built in");
}

#[test]
fn an_account_copy_is_unedited_at_the_uid_it_names_and_a_fixed_one_only_at_its_own() {
    let chrony = entry("chronyd-ntp");
    assert!(is_unedited(Some(chrony), None, &chrony.rule_as(Some(5))));
    assert!(!is_unedited(Some(chrony), None, &chrony.rule()));
    let nm = entry("networkmanager-connectivity-check");
    let mut other = nm.rule();
    other.operator.as_mut().unwrap().list[2].data = "1000".into();
    assert!(!is_unedited(Some(nm), None, &other));
}

#[test]
fn the_unpinned_copy_v1_installed_is_recognised_as_legacy() {
    for id in ROOT_PINNED.into_iter().chain(ACCOUNTS.map(|(id, _)| id)) {
        let entry = entry(id);
        let legacy = entry.legacy_rule().unwrap();
        assert_eq!(uid_leaf(&legacy), None, "{id}");
        assert!(is_legacy(entry, &legacy), "{id}");
        assert!(!is_unedited(Some(entry), None, &legacy), "{id}");
        assert!(!is_legacy(entry, &entry.rule_as(Some(0))), "{id}");
        // Its Rules-page switch still turns it off, never on.
        assert!(toggleable(&legacy), "{id}");
        let mut off = legacy.clone();
        off.enabled = false;
        assert!(!toggleable(&off), "{id}");
    }
}

#[test]
fn each_pinned_entry_says_who_may_send() {
    assert!(entry("networkmanager-connectivity-check")
        .allows()
        .ends_with(", but only while it runs as root (user ID 0)."));
    assert!(entry("chronyd-ntp")
        .allows()
        .ends_with(", but only while it runs as the chrony account."));
    assert!(entry("flatpak-flathub")
        .allows()
        .ends_with("over IPv4 and IPv6."));
}

fn reconcile(rules: &[Rule], choices: &Choices) -> super::reconcile::Plan {
    let rules: BTreeMap<String, Rule> = rules.iter().map(|r| (r.name.clone(), r.clone())).collect();
    let none = BTreeSet::new();
    plan(
        entries(),
        DaemonRules {
            rules: &rules,
            left_out: &none,
            files_left: &none,
            maybe_applied: &none,
        },
        choices,
    )
}

const NM: &str = "networkmanager-connectivity-check";

fn legacy_nm(enabled: bool) -> Rule {
    Rule {
        enabled,
        ..entry(NM).legacy_rule().unwrap()
    }
}

#[test]
fn an_opted_in_legacy_copy_is_upgraded_in_place() {
    let on = Choices {
        enabled: [NM.to_string()].into(),
        ..Choices::default()
    };
    for (enabled, status) in [
        (true, EntryStatus::Installed),
        (false, EntryStatus::InstalledButOff),
    ] {
        let planned = reconcile(&[legacy_nm(enabled)], &on);
        assert_eq!(planned.actions, [CuratedAction::Upgrade(NM.into())]);
        assert_eq!(planned.statuses[NM], status);
    }
}

#[test]
fn a_legacy_copy_turned_off_is_deleted_and_an_undecided_one_left_alone() {
    let off = Choices {
        disabled: [NM.to_string()].into(),
        ..Choices::default()
    };
    let planned = reconcile(&[legacy_nm(true)], &off);
    assert_eq!(
        planned.actions,
        [CuratedAction::Delete {
            id: NM.into(),
            name: entry(NM).rule_name()
        }]
    );
    let planned = reconcile(&[legacy_nm(true)], &Choices::default());
    assert!(planned.actions.is_empty());
    assert_eq!(planned.statuses[NM], EntryStatus::InFirewall);
    let rules: BTreeMap<String, Rule> = [(entry(NM).rule_name(), legacy_nm(true))].into();
    let none = BTreeSet::new();
    let inert = inert_statuses(
        entries(),
        DaemonRules {
            rules: &rules,
            left_out: &none,
            files_left: &none,
            maybe_applied: &none,
        },
    );
    assert_eq!(inert[NM], EntryStatus::InFirewall);
}
