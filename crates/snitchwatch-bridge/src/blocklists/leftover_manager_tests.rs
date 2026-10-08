//! The manager's view of leftover rules (issue #73): reported only when
//! nothing manages them, removable on request, announced when the count
//! changes.

use std::sync::Arc;
use std::time::Duration;

use super::daemon_sink::tests::{user_rule, Daemon, Harness, ADS};
use super::daemon_sink::DaemonRuleSink;
use super::leftover::LeftoverRules;
use super::materializer::ListKind;
use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;
use crate::ws_messages::StorageStatus;

fn leftover(h: &Harness) -> LeftoverRules {
    LeftoverRules::new(h.commands.clone(), h.rules.cache()).with_timeout(Duration::from_millis(300))
}

/// A daemon holding two of the bridge's rules for `ADS`.
fn holding_two() -> Harness {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule(ADS, ListKind::Domains),
        h.bridge_rule(ADS, ListKind::Ips),
        user_rule("899-firefox"),
    ];
    h.connect(Daemon::Accept, snapshot)
}

fn without_a_state_directory(h: &Harness) -> BlocklistsManager {
    BlocklistsManager::new(Arc::new(BlocklistStore::open_in_memory().unwrap()))
        .with_rule_sink(Arc::new(NoopRuleSink::new("no state directory")))
        .with_leftover_rules(leftover(h))
}

#[tokio::test]
async fn rules_nothing_manages_are_leftovers() {
    let h = holding_two();
    assert_eq!(without_a_state_directory(&h).leftover_count(), Some(2));
}

#[tokio::test]
async fn a_healthy_bridge_has_no_leftovers_its_orphans_are_deleted_on_their_own() {
    let h = holding_two();
    let sink = DaemonRuleSink::new(h.dir.clone(), h.commands.clone(), h.rules.cache());
    let mgr = BlocklistsManager::new(Arc::new(BlocklistStore::open_in_memory().unwrap()))
        .with_rule_sink(Arc::new(sink))
        .with_leftover_rules(leftover(&h));
    assert_eq!(mgr.leftover_count(), None);
}

#[tokio::test]
async fn nothing_is_reported_while_the_daemons_rule_list_is_unknown() {
    let h = Harness::new();
    assert_eq!(without_a_state_directory(&h).leftover_count(), None);
}

#[tokio::test]
async fn a_daemon_with_only_user_rules_has_no_leftover_count() {
    let h = Harness::new().connect(Daemon::Accept, vec![user_rule("899-firefox")]);
    assert_eq!(without_a_state_directory(&h).leftover_count(), None);
}

#[tokio::test]
async fn an_unreadable_store_makes_every_rule_of_the_bridge_a_leftover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blocklists.sqlite3");
    BlocklistStore::open(&path)
        .unwrap()
        .upsert_subscription(&Subscription {
            id: "ads".into(),
            url: "https://example.invalid/ads".into(),
            display_name: "ads".into(),
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_attempt_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        })
        .unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE subscriptions SET refresh_interval_secs = 'x'", [])
        .unwrap();
    let h = holding_two();
    let sink = DaemonRuleSink::new(h.dir.clone(), h.commands.clone(), h.rules.cache());
    let mgr = BlocklistsManager::new(Arc::new(BlocklistStore::open(&path).unwrap()))
        .with_rule_sink(Arc::new(sink))
        .with_storage_status(StorageStatus {
            unreadable: false,
            persistent: true,
            reason: None,
        })
        .with_leftover_rules(leftover(&h));
    assert!(mgr.storage_status().unreadable);
    assert_eq!(mgr.leftover_count(), Some(2));
}

#[tokio::test]
async fn removing_deletes_the_rules_and_tells_the_guis() {
    let h = holding_two();
    let mgr = without_a_state_directory(&h);
    let mut events = mgr.subscribe();
    mgr.remove_leftover_rules().await;
    assert_eq!(mgr.leftover_count(), None);
    let sent = h.seen().len();
    assert_eq!(sent, 2, "a delete for each, nothing for the user's rule");
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::SubscriptionsChanged)
    ));
}

#[tokio::test]
async fn a_healthy_bridge_refuses_to_remove_what_it_manages() {
    let h = holding_two();
    let sink = DaemonRuleSink::new(h.dir.clone(), h.commands.clone(), h.rules.cache());
    let mgr = BlocklistsManager::new(Arc::new(BlocklistStore::open_in_memory().unwrap()))
        .with_rule_sink(Arc::new(sink))
        .with_leftover_rules(leftover(&h));
    mgr.remove_leftover_rules().await;
    assert!(h.seen().is_empty());
}

#[tokio::test]
async fn a_change_in_the_count_is_announced_once_by_the_next_reconcile() {
    let h = holding_two();
    let mgr = without_a_state_directory(&h);
    let mut events = mgr.subscribe();
    mgr.reconcile().await;
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::SubscriptionsChanged)
    ));
    mgr.reconcile().await;
    assert!(events.try_recv().is_err(), "the same count isn't news");
}

#[tokio::test]
async fn without_a_leftover_source_there_is_nothing_to_report_or_remove() {
    let mgr = BlocklistsManager::new(Arc::new(BlocklistStore::open_in_memory().unwrap()));
    assert_eq!(mgr.leftover_count(), None);
    mgr.remove_leftover_rules().await;
}

#[tokio::test]
async fn the_event_pump_follows_every_summary_with_the_leftover_count() {
    let h = holding_two();
    let mgr = Arc::new(without_a_state_directory(&h));
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let pump = spawn_event_pump(mgr.clone(), tx);
    mgr.announce_subscriptions();
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no message")
            .unwrap();
        seen.push(msg);
    }
    pump.abort();
    assert!(matches!(
        seen[0],
        crate::ws_messages::ServerMessage::SetBlocklists { .. }
    ));
    assert_eq!(
        seen[1],
        crate::ws_messages::ServerMessage::SetBlocklistLeftovers {
            count: 2,
            cause: Some("no_state_dir".into()),
            reason: None,
        }
    );
}

#[tokio::test]
async fn the_gui_message_runs_the_removal_on_the_worker() {
    use crate::blocklists::worker::BlocklistWorker;
    use crate::ws_messages::ClientMessage;
    let h = holding_two();
    let mgr = Arc::new(without_a_state_directory(&h));
    let mut events = mgr.subscribe();
    let (worker, task) = BlocklistWorker::spawn(mgr.clone());
    assert!(worker
        .try_route(ClientMessage::RemoveLeftoverBlocklistRules)
        .is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        while mgr.leftover_count().is_some() {
            let _ = events.recv().await;
        }
    })
    .await
    .expect("the leftover rules were not removed");
    task.abort();
    assert_eq!(h.seen().len(), 2);
}

fn message(mgr: &BlocklistsManager) -> crate::ws_messages::ServerMessage {
    crate::translator::downstream::build_set_blocklist_leftovers(mgr)
}

fn cause_and_reason(mgr: &BlocklistsManager) -> (Option<String>, Option<String>, u32) {
    match message(mgr) {
        crate::ws_messages::ServerMessage::SetBlocklistLeftovers {
            count,
            cause,
            reason,
        } => (cause, reason, count),
        other => panic!("expected SetBlocklistLeftovers, got {other:?}"),
    }
}

/// With an unreadable store the leftovers are probably lists still
/// subscribed to; the GUI is told so, apart from the other causes.
#[tokio::test]
async fn the_message_says_why_nothing_manages_the_rules() {
    let h = holding_two();
    assert_eq!(
        cause_and_reason(&without_a_state_directory(&h)),
        (Some("no_state_dir".into()), None, 2)
    );
    let per_user = BlocklistsManager::new(Arc::new(BlocklistStore::open_in_memory().unwrap()))
        .with_rule_sink(Arc::new(NoopRuleSink::new(PER_USER_REASON)))
        .with_leftover_rules(leftover(&h));
    assert_eq!(cause_and_reason(&per_user).0.as_deref(), Some("per_user"));
}

#[tokio::test]
async fn an_unreadable_store_is_the_cause_when_the_store_cant_be_read() {
    let h = holding_two();
    // A store that opens but can't be listed.
    let broken = Arc::new(BlocklistStore::open_in_memory().unwrap());
    broken
        .lock_for_test()
        .execute_batch("DROP TABLE subscriptions;")
        .unwrap();
    let mgr = BlocklistsManager::new(broken)
        .with_rule_sink(Arc::new(NoopRuleSink::new("no state directory")))
        .with_leftover_rules(leftover(&h));
    assert_eq!(mgr.leftover_cause(), Some("store_unreadable"));
    assert_eq!(
        cause_and_reason(&mgr).0.as_deref(),
        Some("store_unreadable")
    );
}

/// No leftovers, no cause: the notice is gone, whatever the bridge is.
#[tokio::test]
async fn no_leftovers_have_no_cause() {
    let h = Harness::new().connect(Daemon::Accept, vec![user_rule("899-firefox")]);
    assert_eq!(
        cause_and_reason(&without_a_state_directory(&h)),
        (None, None, 0)
    );
}

/// A removal the daemon partly refuses says so under the button.
#[tokio::test]
async fn a_refused_removal_is_reported_to_the_gui() {
    let h = holding_two();
    h.set_daemon(Daemon::RefuseDelete("busy"));
    let mgr = without_a_state_directory(&h);
    mgr.remove_leftover_rules().await;
    let (cause, reason, count) = cause_and_reason(&mgr);
    assert_eq!((cause.as_deref(), count), (Some("no_state_dir"), 2));
    let reason = reason.expect("the refusal is reported");
    assert!(
        reason.contains("refused") && reason.contains("2"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_removal_that_could_not_reach_the_daemon_says_why() {
    let h = holding_two();
    h.set_daemon(Daemon::Silent);
    let mgr = without_a_state_directory(&h);
    mgr.remove_leftover_rules().await;
    let reason = cause_and_reason(&mgr).1.expect("the failure is reported");
    assert!(
        reason.starts_with("The rules were not removed: The firewall service didn't answer"),
        "{reason}"
    );
}

/// The note belongs to a removal that left something: a clean one clears it,
/// and it is not shown once nothing is left.
#[tokio::test]
async fn the_note_of_a_failed_removal_goes_with_the_leftovers() {
    let h = holding_two();
    h.set_daemon(Daemon::RefuseDelete("busy"));
    let mgr = without_a_state_directory(&h);
    mgr.remove_leftover_rules().await;
    assert!(mgr.leftover_outcome().is_some());
    h.set_daemon(Daemon::Accept);
    mgr.remove_leftover_rules().await;
    assert_eq!(mgr.leftover_count(), None);
    assert_eq!(mgr.leftover_outcome(), None);
    assert_eq!(cause_and_reason(&mgr), (None, None, 0));
}

fn two_rules(h: &Harness) -> Vec<snitchwatch_proto::protocol::Rule> {
    vec![
        h.bridge_rule(ADS, ListKind::Domains),
        h.bridge_rule(ADS, ListKind::Ips),
    ]
}

/// The leftovers go by some other means (the daemon's list changed): the
/// note about the failed removal is not shown for them, nor for any that
/// come later.
#[tokio::test]
async fn a_failed_removals_note_does_not_outlive_its_leftovers() {
    let h = holding_two();
    h.set_daemon(Daemon::RefuseDelete("busy"));
    let mgr = without_a_state_directory(&h);
    mgr.remove_leftover_rules().await;
    assert!(mgr.leftover_outcome().is_some());

    h.rules.cache().lock().unwrap().replace_all(Vec::new());
    assert_eq!(
        cause_and_reason(&mgr),
        (None, None, 0),
        "no note for no leftovers, even before anything announced the change"
    );
    mgr.reconcile().await;

    h.rules.cache().lock().unwrap().replace_all(two_rules(&h));
    let (_, reason, count) = cause_and_reason(&mgr);
    assert_eq!((count, reason), (2, None), "a new set of leftovers is new");
}
