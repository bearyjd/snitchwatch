//! [`RulesSync::learn_account_names`] (#115): only the bounded snapshot's
//! uids, one batch at a time, late names held by a [`PublishHold`]. The
//! lookups are fakes: tests never read the host's accounts.

use super::*;
use crate::accounts::AccountLookup;
use snitchwatch_proto::protocol::Operator;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::broadcast;

fn with_uid(name: &str, uid: &str) -> Rule {
    Rule {
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: "always".to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "user.name".into(),
            data: uid.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn counting(calls: &Arc<AtomicUsize>) -> AccountLookup {
    let calls = calls.clone();
    Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Some("someone".to_string())
    })
}

fn set_rules(rx: &mut broadcast::Receiver<ServerMessage>) -> usize {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|m| matches!(m, ServerMessage::SetRules { .. }))
        .count()
}

#[tokio::test]
async fn an_over_limit_snapshot_looks_nothing_up() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sync = RulesSync::new(broadcast::channel(8).0);
    let rules = vec![with_uid("a", "958"); MAX_SNAPSHOT_RULES + 1];
    sync.learn_account_names(&counting(&calls), &rules).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_rule_left_out_for_its_size_is_not_looked_up() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sync = RulesSync::new(broadcast::channel(8).0);
    let mut long = with_uid("long", "959");
    long.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
    sync.learn_account_names(&counting(&calls), &[long, with_uid("a", "958")])
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "958 only");
}

/// Late names are a list change like a confirmed command's: while a
/// [`PublishHold`] is alive they wait for it (#115).
#[tokio::test]
async fn late_names_wait_for_a_publish_hold() {
    let (tx, mut rx) = broadcast::channel(16);
    let sync = RulesSync::new(tx);
    lock(&sync.cache).replace_all(vec![with_uid("a", "958")]);
    let hold = sync.hold_publishes();
    let calls = Arc::new(AtomicUsize::new(0));
    sync.learn_account_names(&counting(&calls), &[with_uid("a", "958")])
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(set_rules(&mut rx), 0, "published through the hold");
    drop(hold);
    assert_eq!(set_rules(&mut rx), 1);
}

/// A lookup that panics frees the one-batch slot: the next snapshot looks
/// its uids up again (#115).
#[tokio::test]
async fn a_panicking_lookup_frees_the_slot() {
    let sync = RulesSync::new(broadcast::channel(8).0);
    let panicking: AccountLookup = Arc::new(|_| panic!("NSS module crashed"));
    sync.learn_account_names(&panicking, &[with_uid("a", "958")])
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    sync.learn_account_names(&counting(&calls), &[with_uid("a", "958")])
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// One batch at a time: a snapshot that arrives while a lookup hangs starts
/// none, and its uids are looked up by the running batch once it is free.
#[tokio::test]
async fn a_hung_batch_takes_later_uids_when_it_is_free() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = StdMutex::new(released);
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let looked = seen.clone();
    let lookup: AccountLookup = Arc::new(move |uid| {
        looked.lock().unwrap().push(uid);
        if uid == 958 {
            let _ = released.lock().unwrap().recv();
        }
        None
    });
    let sync = RulesSync::new(broadcast::channel(8).0);
    sync.learn_account_names(&lookup, &[with_uid("a", "958")])
        .await;
    sync.learn_account_names(&lookup, &[with_uid("b", "959")])
        .await;
    assert_eq!(*seen.lock().unwrap(), vec![958], "no second batch");
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while seen.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the queued uid was never looked up");
    assert_eq!(*seen.lock().unwrap(), vec![958, 959]);
}
