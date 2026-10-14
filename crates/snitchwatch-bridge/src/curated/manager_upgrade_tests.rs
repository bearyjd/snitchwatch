//! #120 36/41: sender pins at run time. An installed copy from before the
//! pins is replaced in place; a run-time account is installed at the uid
//! it was found at, and withheld while unknown or stale.

use super::tests::*;
use super::*;
use snitchwatch_proto::protocol::{Action, Rule};

const NM: &str = "networkmanager-connectivity-check";
const NM_RULE: &str = "snitchwatch-default-networkmanager-connectivity-check";
const CHRONY: &str = "chronyd-ntp";
const CHRONY_RULE: &str = "snitchwatch-default-chronyd-ntp";

fn entry(id: &str) -> &'static super::super::CuratedEntry {
    entries().iter().find(|entry| entry.id == id).unwrap()
}

fn legacy(id: &str, enabled: bool) -> Rule {
    Rule {
        enabled,
        created: 1_700_000_000,
        ..entry(id).legacy_rule().unwrap()
    }
}

fn uid_sent(harness: &Harness, index: usize) -> String {
    let seen = harness.seen.lock().unwrap();
    let op = seen[index].rules[0].operator.clone().unwrap();
    op.list
        .into_iter()
        .find(|leaf| leaf.operand == "user.id")
        .map(|leaf| leaf.data)
        .unwrap_or_default()
}

/// The user turned `id` on, over the daemon's `snapshot`.
async fn opted_in(id: &str, daemon: Daemon, snapshot: Vec<Rule>) -> (Harness, CuratedDefaults) {
    let harness = Harness::new().connect(daemon, snapshot);
    let curated = harness.curated();
    turn(&curated, id, true);
    (harness, curated)
}

/// One `CHANGE_RULE` of the same name replaces the copy, keeping its
/// `enabled` and `created`; never a delete, never twice.
#[tokio::test]
async fn an_opted_in_unpinned_copy_is_replaced_in_place_once() {
    for enabled in [true, false] {
        let (harness, curated) = opted_in(NM, Daemon::Accept, vec![legacy(NM, enabled)]).await;
        for _ in 0..3 {
            curated.reconcile().await;
        }
        assert_eq!(
            harness.seen(),
            [(Action::ChangeRule as i32, NM_RULE.into())]
        );
        let sent = harness.seen.lock().unwrap()[0].rules[0].clone();
        assert_eq!(sent.enabled, enabled);
        assert_eq!(sent.created, 1_700_000_000);
        assert_eq!(uid_sent(&harness, 0), "0");
        let status = entry_state(&curated, NM).status;
        if enabled {
            assert_eq!(status, EntryStatus::Installed);
        } else {
            assert_eq!(status, EntryStatus::InstalledButOff);
        }
        let saved = store::load(&harness.file).unwrap().unwrap();
        assert!(saved.installed.contains_key(NM), "{saved:?}");
    }
}

/// A refused replacement leaves the entry on and its old copy in place: it
/// is not #130's refused install, and isn't sent again on this stream.
#[tokio::test]
async fn a_refused_replacement_keeps_the_entry_on() {
    let (harness, curated) = opted_in(NM, Daemon::Refuse, vec![legacy(NM, true)]).await;
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        harness.seen(),
        [(Action::ChangeRule as i32, NM_RULE.into())]
    );
    assert!(entry_state(&curated, NM).on);
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(saved.enabled.contains(NM), "{saved:?}");
}

/// Undecided (a first run), the old copy is left as it is.
#[tokio::test]
async fn an_undecided_unpinned_copy_is_left_alone() {
    let harness = Harness::new().connect(Daemon::Accept, vec![legacy(NM, true)]);
    let curated = harness.curated();
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    assert_eq!(entry_state(&curated, NM).status, EntryStatus::InFirewall);
}

/// chronyd is installed pinned to the uid its account was found at.
#[tokio::test]
async fn chronyd_is_installed_at_its_accounts_uid() {
    let (harness, curated) = opted_in(CHRONY, Daemon::Accept, Vec::new()).await;
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::ChangeRule as i32, CHRONY_RULE.into())]
    );
    assert_eq!(uid_sent(&harness, 0), "994");
    assert_eq!(entry_state(&curated, CHRONY).status, EntryStatus::Installed);
}

/// The chrony account unknown: an old copy is neither replaced nor
/// deleted, nothing new is installed, and the entry says why. Turning it
/// off still deletes it.
#[tokio::test]
async fn an_unknown_account_leaves_an_old_copy_as_it_is() {
    let (harness, curated) = opted_in(CHRONY, Daemon::Accept, vec![legacy(CHRONY, true)]).await;
    curated.set_uid_lookup(Arc::new(|name| (name == "systemd-resolve").then_some(990)));
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    let state = entry_state(&curated, CHRONY);
    assert_eq!(state.status, EntryStatus::Unavailable);
    assert_eq!(state.problem.as_deref(), Some(ACCOUNT_REASON));
    assert!(state.on);
    turn(&curated, CHRONY, false);
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::DeleteRule as i32, CHRONY_RULE.into())]
    );
}

/// An undecided copy still applies while its account is unknown: it reads
/// on, with Keep, and the reason is attached; nothing is sent.
#[tokio::test]
async fn an_undecided_copy_held_back_still_reads_as_in_the_firewall() {
    let harness = Harness::new().connect(Daemon::Accept, vec![legacy(CHRONY, true)]);
    let curated = harness.curated();
    curated.set_uid_lookup(Arc::new(|name| (name == "systemd-resolve").then_some(990)));
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    let state = entry_state(&curated, CHRONY);
    assert_eq!(state.status, EntryStatus::InFirewall);
    assert!(state.on);
    assert_eq!(state.problem.as_deref(), Some(ACCOUNT_REASON));
}
