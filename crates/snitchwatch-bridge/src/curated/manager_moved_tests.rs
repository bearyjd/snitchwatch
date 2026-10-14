//! A service account found at another uid than its copy names (the tower's
//! r13: `systemd-resolve` and `chrony` are allocated per image build), under
//! either answer of [`ON_UID_CHANGE`].

use super::intent_tests::{prepare, submit};
use super::tests::*;
use super::*;
use crate::curated::intent::IntentContext;
use crate::ws_messages::CuratedIntentPurpose as Purpose;
use snitchwatch_proto::protocol::{Action, Rule};

const CHRONY: &str = "chronyd-ntp";
const CHRONY_RULE: &str = "snitchwatch-default-chronyd-ntp";
const DNS: &str = "dns-resolved";
const DNS_RULE: &str = "snitchwatch-default-dns-resolved";

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

fn set_policy(curated: &CuratedDefaults, policy: UidChange) {
    lock(&curated.inner.state).uid_change = policy;
}

/// The placeholder answer is A.
#[test]
fn the_placeholder_answer_is_rewrite() {
    assert_eq!(ON_UID_CHANGE, UidChange::Rewrite);
}

/// A: an opted-in chrony copy at the old uid is rewritten in place at the
/// new one, keeping `enabled`; once, never deleted.
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
        assert!(entry_state(&curated, CHRONY).on);
    }
}

/// A: the DNS entry's copy at the old uid is rewritten under its live
/// grant, through the guarded broad send.
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

/// A: without a live grant (a grant made for the old fixed uid no longer
/// matches), the enabled DNS copy isn't rewritten; the entry says why. A new
/// confirmation then rewrites it, once.
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

/// B: the entry is turned off, with a note, and its copy then deleted; its
/// grant goes with it.
#[tokio::test]
async fn under_turn_off_a_moved_account_is_turned_off_and_deleted() {
    let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 993, true)]);
    let curated = harness.curated();
    set_policy(&curated, UidChange::TurnOff);
    turn(&curated, CHRONY, true);
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        harness.seen(),
        [(Action::DeleteRule as i32, CHRONY_RULE.into())]
    );
    let state = entry_state(&curated, CHRONY);
    assert!(!state.on);
    assert_eq!(state.problem.as_deref(), Some(UID_CHANGED_OFF_REASON));
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(!saved.enabled.contains(CHRONY));
}

/// B, DNS: its grant is revoked too, so turning it on asks again.
#[tokio::test]
async fn under_turn_off_a_moved_dns_entry_loses_its_grant() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    set_policy(&curated, UidChange::TurnOff);
    opt_in_dns(&curated, Purpose::Enable);
    curated.reconcile().await;
    harness.resync(vec![at(DNS, 193, true)]);
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(
        harness.seen(),
        [
            (Action::ChangeRule as i32, DNS_RULE.into()),
            (Action::DeleteRule as i32, DNS_RULE.into()),
        ]
    );
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(!saved.broad_grants.contains_key(DNS), "{saved:?}");
}

/// Either answer: an undecided copy at a moved uid is left as it is, and
/// says why.
#[tokio::test]
async fn an_undecided_moved_copy_is_left_alone() {
    for policy in [UidChange::Rewrite, UidChange::TurnOff] {
        let harness = Harness::new().connect(Daemon::Accept, vec![at(CHRONY, 993, true)]);
        let curated = harness.curated();
        set_policy(&curated, policy);
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
