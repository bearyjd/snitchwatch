//! #130: a refused install is turned off, its consent dropped, and the name
//! deleted once.

use super::intent_tests::{prepare, submit};
use super::tests::*;
use super::*;
use crate::curated::intent::IntentContext;
use crate::ws_messages::CuratedIntentPurpose as Purpose;
use snitchwatch_proto::protocol::Action;

const DNS: &str = "dns-resolved";
const DNS_RULE: &str = "snitchwatch-default-dns-resolved";

/// The broad DNS entry, refused: its grant is dropped from the saved file,
/// the name is deleted once, and turning it on again needs a new ticket.
#[tokio::test]
async fn a_refused_dns_install_drops_its_grant_and_needs_a_new_ticket() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&curated, &context, Purpose::Enable);
    submit(&curated, Some(context), ticket, Purpose::Enable, None);
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::ChangeRule as i32, DNS_RULE.into())]
    );
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(!saved.broad_grants.contains_key(DNS), "{saved:?}");
    assert!(!saved.enabled.contains(DNS));
    assert!(saved.disabled.contains(DNS));

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
    let state = entry_state(&curated, DNS);
    assert!(!state.on);
    assert_eq!(state.status, EntryStatus::Off);
    assert_eq!(state.problem.as_deref(), Some(INSTALL_REFUSED_REASON));

    // A plain "on" is no consent; a reconnect doesn't bring it back either.
    turn(&curated, DNS, true);
    harness.resync(Vec::new());
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 2, "{:?}", harness.seen());
}

/// Turned on again by the user, the note goes and the install is retried.
#[tokio::test]
async fn turning_a_refused_entry_on_again_forgets_why_it_was_off() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    curated.reconcile().await;
    *harness.policy.lock().unwrap() = Daemon::Accept;
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::Installed);
    assert!(state.on);
    assert_eq!(state.problem, None);
}

/// The same shape for the DNS entry's explicit re-enable of its rule, off
/// on the Rules page: refused, the daemon may apply it enabled while the
/// list shows it off. Its grant is dropped and it is turned off again, once.
#[tokio::test]
async fn a_refused_dns_reenable_drops_its_grant_and_is_turned_off_again() {
    let mut disabled = entries()
        .iter()
        .find(|entry| entry.id == DNS)
        .unwrap()
        .rule();
    disabled.enabled = false;
    let harness = Harness::new().connect(Daemon::Accept, vec![disabled]);
    let curated = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&curated, &context, Purpose::Keep);
    submit(&curated, Some(context.clone()), ticket, Purpose::Keep, None);
    curated.reconcile().await;
    *harness.policy.lock().unwrap() = Daemon::Refuse;
    let ticket = prepare(&curated, &context, Purpose::Reenable);
    submit(&curated, Some(context), ticket, Purpose::Reenable, None);
    curated.reconcile().await;
    for _ in 0..3 {
        curated.reconcile().await;
    }
    let sent: Vec<bool> = harness
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| {
            assert_eq!(n.r#type, Action::ChangeRule as i32);
            n.rules[0].enabled
        })
        .collect();
    assert_eq!(sent, [true, false]);
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(!saved.broad_grants.contains_key(DNS), "{saved:?}");
    assert_eq!(
        entry_state(&curated, DNS).status,
        EntryStatus::InstalledButOff
    );
}
