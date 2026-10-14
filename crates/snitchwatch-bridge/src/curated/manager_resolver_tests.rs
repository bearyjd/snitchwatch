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

/// The lookup fails, or finds another uid (stale, #155), or no such
/// account; with the reason each shows.
fn unconfirmed() -> [(crate::accounts::UidLookup, &'static str); 3] {
    [
        (Arc::new(|_| None), DNS_ACCOUNT_REASON),
        (Arc::new(|_| Some(991)), DNS_STALE_REASON),
        (
            Arc::new(|name| (name != "systemd-resolve").then_some(193)),
            DNS_ACCOUNT_REASON,
        ),
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
    for (lookup, reason) in unconfirmed() {
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
        assert_eq!(state.problem.as_deref(), Some(reason));
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
    for (lookup, _) in unconfirmed() {
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

/// #149 review 4: a lookup that hangs (an LDAP or SSSD backend, say) doesn't
/// stall the pass: after `RESOLVER_LOOKUP_TIMEOUT` the account counts as
/// unconfirmed, and the pass goes on.
#[tokio::test]
async fn a_hung_lookup_counts_as_unconfirmed_and_doesnt_stall_the_pass() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    opt_in(&curated);
    let (release, hung) = std::sync::mpsc::channel::<()>();
    let hung = Arc::new(Mutex::new(hung));
    curated.set_uid_lookup(Arc::new(move |_| {
        let _ = hung.lock().unwrap().recv();
        Some(193)
    }));
    let pass = tokio::time::timeout(RESOLVER_LOOKUP_TIMEOUT * 3, curated.reconcile()).await;
    release.send(()).unwrap();
    assert!(pass.is_ok(), "the pass waited on the lookup");
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    assert_eq!(entry_state(&curated, DNS).status, EntryStatus::Unavailable);
}

/// #155: a definite answer that systemd-resolve isn't 193 is not a passing
/// failure: an installed copy reads stale, with a fixed reason saying how to
/// remove it, and is still not deleted (item 38's rule).
#[tokio::test]
async fn a_definite_uid_mismatch_reads_stale_and_deletes_nothing() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    opt_in(&curated);
    curated.reconcile().await;
    harness.resync(vec![dns_rule()]);
    curated.set_uid_lookup(Arc::new(|_| Some(991)));
    for _ in 0..3 {
        curated.reconcile().await;
    }
    assert_eq!(harness.seen().len(), 1, "{:?}", harness.seen());
    let state = entry_state(&curated, DNS);
    assert_eq!(state.status, EntryStatus::Unavailable);
    assert_eq!(state.problem.as_deref(), Some(DNS_STALE_REASON));
    assert!(state.on);
}

/// A lookup that blocks until released, counting its calls.
fn hung_lookup() -> (
    crate::accounts::UidLookup,
    Arc<std::sync::atomic::AtomicUsize>,
    std::sync::mpsc::Sender<()>,
) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release, hung) = std::sync::mpsc::channel::<()>();
    let hung = Arc::new(Mutex::new(hung));
    let counted = calls.clone();
    let lookup: crate::accounts::UidLookup = Arc::new(move |name| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = hung.lock().unwrap().recv();
        (name == "systemd-resolve").then_some(193)
    });
    (lookup, calls, release)
}

/// #155: with the account database hung, one lookup is in flight at a
/// time. Later passes reuse the last result at once, and start no other
/// blocking thread to strand.
#[tokio::test]
async fn a_hung_lookup_is_not_started_again_by_later_passes() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    opt_in(&curated);
    let (lookup, calls, release) = hung_lookup();
    curated.set_uid_lookup(lookup);
    curated.reconcile().await;
    for _ in 0..3 {
        let pass = tokio::time::timeout(Duration::from_secs(1), curated.reconcile()).await;
        assert!(pass.is_ok(), "a later pass waited on the hung lookup");
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(release);
    assert_eq!(entry_state(&curated, DNS).status, EntryStatus::Unavailable);
}

/// #155: a lookup that answers after its pass gave up is not lost: the
/// answer changes what a pass reads, so the worker plans again.
#[tokio::test]
async fn a_late_lookup_answer_changes_the_pass_key() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    opt_in(&curated);
    let (lookup, _, release) = hung_lookup();
    curated.set_uid_lookup(lookup);
    curated.reconcile().await;
    let before = curated.pass_key();
    release.send(()).unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(5), async {
        while curated.pass_key() == before {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    assert!(changed.is_ok(), "the late answer was dropped");
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::ChangeRule as i32, DNS_RULE.into())]
    );
}

/// #157: a manager nobody gave a lookup never reads the host's accounts:
/// every account is unknown, so the DNS entry is withheld.
#[tokio::test]
async fn a_new_manager_confirms_no_account_until_given_a_lookup() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = CuratedDefaults::new(
        harness.commands.clone(),
        harness.rules.cache(),
        harness.broadcast.clone(),
    );
    curated.attach_file(harness.file.clone());
    curated.reconcile().await;
    assert_eq!(entry_state(&curated, DNS).status, EntryStatus::Unavailable);
    assert!(lock(&curated.inner.state).withheld.contains(DNS));
}

/// #157: a bridge that changes no recommended rules looks no account up.
#[tokio::test]
async fn an_inert_bridge_looks_no_account_up() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    curated.set_unavailable("per-user");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    curated.set_uid_lookup(Arc::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(193)
    }));
    curated.reconcile().await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}
