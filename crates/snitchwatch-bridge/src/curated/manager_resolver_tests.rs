//! #120 item 38: the DNS entry only where systemd-resolve is uid 193.

use super::intent_tests::{prepare, submit};
use super::tests::*;
use super::*;
use crate::curated::intent::IntentContext;
use crate::ws_messages::{CuratedIntentPurpose as Purpose, CuratedRequestOutcome, ReplyTo};
use snitchwatch_proto::protocol::Action;

const DNS: &str = "dns-resolved";
const DNS_RULE: &str = "snitchwatch-default-dns-resolved";

fn dns_rule() -> snitchwatch_proto::protocol::Rule {
    entries().iter().find(|e| e.id == DNS).unwrap().rule()
}

/// The lookup fails, or finds another uid, or no such account.
fn unconfirmed() -> [crate::accounts::UidLookup; 3] {
    [
        Arc::new(|_| None),
        Arc::new(|_| Some(991)),
        Arc::new(|name| (name != "systemd-resolve").then_some(193)),
    ]
}

/// The user opted the DNS entry in, with its ticket.
fn opt_in(curated: &CuratedDefaults) {
    let context = IntentContext::for_test();
    let ticket = prepare(curated, &context, Purpose::Enable);
    submit(curated, Some(context), ticket, Purpose::Enable, None);
}

/// Unconfirmed: an opted-in entry isn't installed, reads unavailable and
/// says why; its choice and grant stay, so it is installed once confirmed.
#[tokio::test]
async fn an_opted_in_dns_entry_waits_for_its_account_to_be_confirmed() {
    for lookup in unconfirmed() {
        let harness = Harness::new().connect(Daemon::Accept, Vec::new());
        let curated = harness.curated();
        opt_in(&curated);
        curated.set_uid_lookup(lookup);
        for _ in 0..3 {
            curated.reconcile().await;
        }
        assert!(harness.seen().is_empty(), "{:?}", harness.seen());
        let state = entry_state(&curated, DNS);
        assert_eq!(state.status, EntryStatus::Unavailable);
        assert_eq!(state.problem.as_deref(), Some(DNS_ACCOUNT_REASON));
        let saved = store::load(&harness.file).unwrap().unwrap();
        assert!(saved.has_broad_authority(DNS));

        curated.set_uid_lookup(Arc::new(|_| Some(193)));
        curated.reconcile().await;
        assert_eq!(
            harness.seen(),
            [(Action::ChangeRule as i32, DNS_RULE.into())]
        );
        assert_eq!(entry_state(&curated, DNS).status, EntryStatus::Installed);
    }
}

/// Unconfirmed: an installed copy the user opted into is never deleted,
/// and its choices are kept.
#[tokio::test]
async fn an_installed_dns_rule_is_left_as_it_is_while_unconfirmed() {
    for lookup in unconfirmed() {
        let harness = Harness::new().connect(Daemon::Accept, Vec::new());
        let curated = harness.curated();
        opt_in(&curated);
        curated.reconcile().await;
        assert_eq!(harness.seen().len(), 1);
        let before = store::load(&harness.file).unwrap().unwrap();
        curated.set_uid_lookup(lookup);
        harness.resync(vec![dns_rule()]);
        for _ in 0..3 {
            curated.reconcile().await;
        }
        assert_eq!(harness.seen().len(), 1, "{:?}", harness.seen());
        assert_eq!(entry_state(&curated, DNS).status, EntryStatus::Unavailable);
        assert_eq!(store::load(&harness.file).unwrap().unwrap(), before);
    }
}

/// Unconfirmed, a new opt-in is refused as unavailable.
#[tokio::test]
async fn a_dns_opt_in_is_refused_while_unconfirmed() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    curated.set_uid_lookup(Arc::new(|_| None));
    curated.reconcile().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    curated.try_route(ClientMessage::PrepareCuratedBroadOptIn {
        request_id: "intent-test".into(),
        id: DNS.into(),
        purpose: Purpose::Enable,
        context: Some(IntentContext::for_test()),
        reply: Some(ReplyTo::new(tx)),
    });
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.try_recv().unwrap() else {
        panic!("a ticket while unconfirmed")
    };
    assert_eq!(results[0].outcome, CuratedRequestOutcome::Unavailable);
}

/// Turning it off still removes it: that is the user's own choice.
#[tokio::test]
async fn turning_the_dns_entry_off_still_removes_it_while_unconfirmed() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    opt_in(&curated);
    curated.reconcile().await;
    harness.resync(vec![dns_rule()]);
    curated.set_uid_lookup(Arc::new(|_| None));
    curated.reconcile().await;
    turn(&curated, DNS, false);
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [
            (Action::ChangeRule as i32, DNS_RULE.into()),
            (Action::DeleteRule as i32, DNS_RULE.into()),
        ]
    );
}
