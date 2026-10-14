//! A service account found at another uid than its copy names (the tower's
//! r13: `systemd-resolve` and `chrony` are allocated per image build): the
//! owner's answer A, re-pinned in place (2026-10-14).

use super::intent_tests::{prepare, submit};
use super::tests::*;
use super::*;
use crate::curated::intent::IntentContext;
use crate::ws_messages::CuratedIntentPurpose as Purpose;
use snitchwatch_proto::protocol::{Action, Rule};

const CHRONY: &str = "chronyd-ntp";
const CHRONY_RULE: &str = "snitchwatch-default-chronyd-ntp";
const DNS: &str = "dns-resolved";

fn at(id: &str, uid: u32, enabled: bool) -> Rule {
    Rule {
        enabled,
        created: 1_700_000_000,
        ..entries()
            .iter()
            .find(|entry| entry.id == id)
            .unwrap()
            .rule_as(Some(uid))
    }
}

/// What was sent: kind, rule's `enabled`, and the uid it names.
fn sent(harness: &Harness) -> Vec<(i32, bool, String)> {
    harness
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| {
            let rule = &n.rules[0];
            let uid = rule
                .operator
                .as_ref()
                .and_then(|op| op.list.iter().find(|l| l.operand == "user.id"))
                .map(|l| l.data.clone())
                .unwrap_or_default();
            (n.r#type, rule.enabled, uid)
        })
        .collect()
}

fn opt_in_dns(curated: &CuratedDefaults, purpose: Purpose) {
    let context = IntentContext::for_test();
    let ticket = prepare(curated, &context, purpose);
    submit(curated, Some(context), ticket, purpose, None);
}

/// An opted-in chrony copy at the old uid is rewritten in place at the new
/// one, keeping `enabled`; once, never deleted; with a short note.
#[tokio::test]
async fn a_moved_account_is_rewritten_in_place() {
    for enabled in [true, false] {
        let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 993, enabled)]);
        let curated = harness.curated();
        turn(&curated, CHRONY, true);
        for _ in 0..3 {
            curated.reconcile().await;
        }
        assert_eq!(
            sent(&harness),
            [(Action::ChangeRule as i32, enabled, "994".into())]
        );
        assert_eq!(
            harness.seen.lock().unwrap()[0].rules[0].created,
            1_700_000_000
        );
        let state = entry_state(&curated, CHRONY);
        assert!(state.on);
        assert_eq!(state.problem.as_deref(), Some(REPINNED_NOTE));
    }
}

/// The DNS entry's copy at the old uid is rewritten under its grant,
/// through the guarded broad send: the grant names the account.
#[tokio::test]
async fn a_moved_dns_account_is_rewritten_under_its_grant() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    opt_in_dns(&curated, Purpose::Enable);
    curated.reconcile().await;
    harness.resync(vec![at(DNS, 990, true)]);
    curated.set_uid_lookup(Arc::new(|name| (name == "systemd-resolve").then_some(989)));
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        sent(&harness),
        [
            (Action::ChangeRule as i32, true, "990".into()),
            (Action::ChangeRule as i32, true, "989".into()),
        ]
    );
}

/// Without any grant, the enabled DNS copy isn't rewritten; the entry says
/// why. A new confirmation then rewrites it, once.
#[tokio::test]
async fn a_moved_dns_copy_without_its_grant_waits_for_a_new_confirmation() {
    let harness = Harness::new().connect(Daemon::Accept, vec![at(DNS, 193, true)]);
    let curated = harness.curated();
    {
        let mut state = lock(&curated.inner.state);
        let choices = state.choices.enable(DNS);
        keep(&mut state, choices);
    }
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    let state = entry_state(&curated, DNS);
    assert_eq!(state.problem.as_deref(), Some(DNS_MOVED_REASON));
    assert!(state.on);
    opt_in_dns(&curated, Purpose::Enable);
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        sent(&harness),
        [(Action::ChangeRule as i32, true, "990".into())]
    );
}

/// An undecided copy at a moved uid is left as it is, and says why.
#[tokio::test]
async fn an_undecided_moved_copy_is_left_alone() {
    {
        let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 993, true)]);
        let curated = harness.curated();
        curated.reconcile().await;
        assert!(harness.seen().is_empty(), "{:?}", harness.seen());
        let state = entry_state(&curated, CHRONY);
        assert_eq!(state.status, EntryStatus::InFirewall);
        assert_eq!(state.problem.as_deref(), Some(MOVED_UNDECIDED_REASON));
    }
}

/// A moved copy is never turned on: not from the Rules page, and not by a
/// queued re-enable. Turning it off still is.
#[tokio::test]
async fn a_moved_copy_is_never_turned_on() {
    let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 993, false)]);
    let curated = harness.curated();
    curated.reconcile().await;
    assert!(curated.send_managed_toggle(CHRONY_RULE, true).is_err());

    let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 993, true)]);
    let curated = harness.curated();
    curated.reconcile().await;
    assert!(curated.send_managed_toggle(CHRONY_RULE, false).is_ok());

    // DNS, off on the Rules page at the old uid: a re-enable intent is
    // dropped; only the rewrite (still off) is sent.
    let harness = Harness::new().connect(Daemon::Accept, vec![at(DNS, 193, false)]);
    let curated = harness.curated();
    opt_in_dns(&curated, Purpose::Keep);
    opt_in_dns(&curated, Purpose::Reenable);
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        sent(&harness),
        [(Action::ChangeRule as i32, false, "990".into())]
    );
}

/// A refused rewrite takes #130's path: one rewrite, then the entry is
/// off, its copy deleted (the old uid may be another account's), and the
/// refusal shown. Nothing loops.
#[tokio::test]
async fn a_refused_rewrite_turns_the_entry_off_and_deletes_it_once() {
    let harness = Harness::new().connect(Daemon::Refuse, vec![at(CHRONY, 993, true)]);
    let curated = harness.curated();
    turn(&curated, CHRONY, true);
    for _ in 0..4 {
        curated.reconcile().await;
    }
    assert_eq!(
        harness.seen(),
        [
            (Action::ChangeRule as i32, CHRONY_RULE.into()),
            (Action::DeleteRule as i32, CHRONY_RULE.into()),
        ]
    );
    let state = entry_state(&curated, CHRONY);
    assert!(!state.on);
    assert_eq!(state.problem.as_deref(), Some(INSTALL_REFUSED_REASON));
}

/// A None between confirmed answers changes nothing; each confirmed change
/// rewrites once.
#[tokio::test]
async fn a_transient_none_neither_rewrites_nor_deletes() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, CHRONY, true);
    curated.reconcile().await;
    let lookup = |uid: Option<u32>| -> crate::accounts::UidLookup {
        Arc::new(move |name| {
            if name == "chrony" {
                uid
            } else {
                fake_accounts(name)
            }
        })
    };
    let mut copy = at(CHRONY, 994, true);
    for (uid, rewrites) in [
        (None, 0),
        (Some(995), 1),
        (None, 0),
        (Some(995), 0),
        (Some(994), 1),
    ] {
        harness.resync(vec![copy.clone()]);
        curated.set_uid_lookup(lookup(uid));
        let before = harness.seen().len();
        for _ in 0..3 {
            curated.reconcile().await;
        }
        let new = harness.seen.lock().unwrap()[before..].to_vec();
        assert_eq!(new.len(), rewrites, "{uid:?}: {new:?}");
        assert!(new.iter().all(|n| n.r#type == Action::ChangeRule as i32));
        if let Some(sent) = new.last() {
            copy = sent.rules[0].clone();
        }
        assert!(entry_state(&curated, CHRONY).on);
    }
}

/// A grant saved for the DNS rule at the uid it was installed with (before
/// grants left the uid out) carries over to the account: the moved copy is
/// rewritten without a new confirmation.
#[tokio::test]
async fn a_grant_from_the_fixed_uid_carries_over() {
    let harness = Harness::new().connect(Daemon::Accept, vec![at(DNS, 193, true)]);
    let old = at(DNS, 193, true);
    let mut choices = store::Choices::default().enable(DNS).installed(DNS, &old);
    let bytes = serde_json::to_vec(&crate::curated::canonical::canonical(&old)).unwrap();
    let grant = serde_json::json!({
        "version": 1, "id": DNS, "consent": store::BROAD_CONSENT,
        "policySha256": format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes)),
        "authority": "enable",
    });
    choices
        .broad_grants
        .insert(DNS.into(), serde_json::from_value(grant).unwrap());
    let raw = serde_json::json!({"version": 3, "choices": choices}).to_string();
    std::fs::write(&harness.file, raw).unwrap();
    std::fs::set_permissions(
        &harness.file,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let curated = harness.curated();
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        sent(&harness),
        [(Action::ChangeRule as i32, true, "990".into())]
    );
}

/// Backstops: the rewrite itself refuses an enabled DNS rewrite without a
/// grant, and the guarded send refuses to replace a copy that isn't the
/// entry's own.
#[tokio::test]
async fn the_rewrites_backstops_hold() {
    let harness = Harness::new().connect(Daemon::Accept, vec![at(DNS, 193, true)]);
    let curated = harness.curated();
    {
        let mut state = lock(&curated.inner.state);
        let choices = state.choices.enable(DNS);
        keep(&mut state, choices);
    }
    curated.reconcile().await;
    let (generation, revision) = (
        curated.generation(),
        lock(&curated.inner.state).broad_revision,
    );
    curated.upgrade(DNS, generation, 0, revision).await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());

    let entry = entries().iter().find(|e| e.id == DNS).unwrap();
    let mut edited = at(DNS, 193, true);
    edited.operator.as_mut().unwrap().list[2].data = "5353".into();
    let harness = Harness::new().connect(Daemon::Accept, vec![edited]);
    let command = crate::daemon_commands::CuratedCommand::authorized_upgrade(
        entry,
        &at(DNS, 193, true),
        Some(990),
    )
    .unwrap();
    let generation = *harness.commands.stream_ready().borrow();
    assert!(harness
        .commands
        .send_curated_guarded_replace(command, generation)
        .is_err());
    // Nor is a copy at the account's uid a move.
    assert!(
        crate::daemon_commands::CuratedCommand::upgrade(entry, &at(DNS, 990, true), Some(990))
            .is_none()
    );
}

/// Security review L1: a guarded replace never turns on a copy that is off
/// now (turned off between the pass's look and the send).
#[tokio::test]
async fn a_guarded_replace_never_turns_on_a_copy_that_is_off() {
    let entry = entries().iter().find(|e| e.id == DNS).unwrap();
    let harness = Harness::new().connect(Daemon::Accept, vec![at(DNS, 193, false)]);
    let generation = *harness.commands.stream_ready().borrow();
    let on = crate::daemon_commands::CuratedCommand::authorized_upgrade(
        entry,
        &at(DNS, 193, true),
        Some(990),
    )
    .unwrap();
    assert!(harness
        .commands
        .send_curated_guarded_replace(on, generation)
        .is_err());
    let off = crate::daemon_commands::CuratedCommand::authorized_upgrade(
        entry,
        &at(DNS, 193, false),
        Some(990),
    )
    .unwrap();
    assert!(harness
        .commands
        .send_curated_guarded_replace(off, generation)
        .is_ok());
}

/// Security review L2: while an entry is withheld (its account not yet
/// confirmed, a first lookup hung), the Rules page can't turn it on; off
/// still works.
#[tokio::test]
async fn a_withheld_entry_is_not_turned_on_from_the_rules_page() {
    let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 994, false)]);
    let curated = harness.curated();
    curated.set_uid_lookup(Arc::new(|_| None));
    curated.reconcile().await;
    assert!(curated.send_managed_toggle(CHRONY_RULE, true).is_err());
    let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 994, true)]);
    let curated = harness.curated();
    curated.set_uid_lookup(Arc::new(|_| None));
    curated.reconcile().await;
    assert!(curated.send_managed_toggle(CHRONY_RULE, false).is_ok());
}

/// #120 item 22 on the rewrite path: the refused rewrite's record of what may
/// apply is stamped with the snapshot's sequence, not the HELLO count.
#[tokio::test]
async fn a_refused_rewrite_record_is_stamped_with_the_snapshot_sequence() {
    let harness = Harness::new().connect(Daemon::Refuse, vec![at(CHRONY, 993, true)]);
    let curated = harness.curated();
    // A second HELLO on the same stream: the generation moves, the snapshot
    // doesn't.
    harness.answer(0, true);
    let epoch = harness.rules.cache().lock().unwrap().snapshot_sequence();
    assert_ne!(curated.generation(), epoch);
    turn(&curated, CHRONY, true);
    curated.reconcile().await;
    let records = lock(&curated.inner.state).maybe_applied.clone();
    assert!(
        !records.is_empty() && records.values().all(|record| record.epoch == epoch),
        "{records:?}"
    );
}
