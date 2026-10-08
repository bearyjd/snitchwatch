//! [`BlocklistsManager::reconcile`] and unsubscribe against a scripted sink
//! (issue #45 PR B).

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;

use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;
use crate::ws_messages::StorageStatus;

#[derive(Default)]
struct ScriptedSink {
    unknown: bool,
    current: Vec<&'static str>,
    verified: Vec<&'static str>,
    reinstalls: StdMutex<Vec<String>>,
    unavailable: Option<&'static str>,
    refuse: Option<&'static str>,
    pushes: StdMutex<Vec<(String, usize)>>,
    removed: StdMutex<Vec<String>>,
    orphan_passes: StdMutex<Vec<Vec<String>>>,
}

#[async_trait]
impl RuleSink for ScriptedSink {
    fn daemon_rules_known(&self) -> bool {
        !self.unknown
    }

    fn is_current(&self, list_id: &str) -> bool {
        self.current.contains(&list_id)
    }

    fn files_verified(&self, list_id: &str) -> bool {
        self.verified.contains(&list_id)
    }

    async fn reinstall_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.reinstalls.lock().unwrap().push(list_id.to_string());
        Ok(())
    }

    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        self.pushes
            .lock()
            .unwrap()
            .push((list_id.to_string(), hosts.len()));
        match (self.unavailable, self.refuse) {
            (Some(reason), _) => Err(NotInstalled::daemon_unavailable(reason)),
            (None, Some(reason)) => Err(NotInstalled::new(reason)),
            (None, None) => Ok(()),
        }
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.removed.lock().unwrap().push(list_id.to_string());
        Ok(())
    }

    async fn remove_orphans(&self, keep: &[String]) {
        self.orphan_passes.lock().unwrap().push(keep.to_vec());
    }
}

/// `downloaded` lists with two hosts each, plus one never downloaded.
fn manager(sink: Arc<ScriptedSink>, downloaded: &[&str]) -> BlocklistsManager {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for (id, fetched) in downloaded
        .iter()
        .map(|id| (*id, true))
        .chain([("never", false)])
    {
        store
            .upsert_subscription(&Subscription {
                id: id.into(),
                url: format!("https://example.invalid/{id}"),
                display_name: id.into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: fetched.then(Utc::now),
                last_attempt_at: None,
                last_fetch_status: if fetched {
                    FetchStatus::Ok
                } else {
                    FetchStatus::Pending
                },
                entry_count: if fetched { 2 } else { 0 },
            })
            .unwrap();
        if fetched {
            store
                .replace_entries(id, &["a.example", "b.example"])
                .unwrap();
        }
    }
    BlocklistsManager::new(store).with_rule_sink(sink)
}

fn pushed(sink: &ScriptedSink) -> Vec<(String, usize)> {
    sink.pushes.lock().unwrap().clone()
}

#[tokio::test]
async fn nothing_happens_while_the_daemons_rule_list_is_unknown() {
    let sink = Arc::new(ScriptedSink {
        unknown: true,
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile().await;
    assert!(pushed(&sink).is_empty());
    assert!(sink.orphan_passes.lock().unwrap().is_empty());
    assert_eq!(mgr.enforcement("ads"), Enforcement::Pending);
}

#[tokio::test]
async fn every_downloaded_list_not_installed_is_pushed_from_the_store() {
    let sink = Arc::new(ScriptedSink::default());
    let mgr = manager(sink.clone(), &["ads", "trackers"]);
    let mut events = mgr.subscribe();
    mgr.reconcile().await;
    assert_eq!(
        pushed(&sink),
        vec![("ads".to_string(), 2), ("trackers".to_string(), 2)],
        "the never-downloaded list is left to the refresh loop"
    );
    for id in ["ads", "trackers"] {
        assert!(matches!(
            mgr.enforcement(id),
            Enforcement::RuleInstalled { .. }
        ));
    }
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::StatusChanged { .. })
    ));
    assert_eq!(
        *sink.orphan_passes.lock().unwrap(),
        vec![vec![
            "ads".to_string(),
            "never".to_string(),
            "trackers".to_string()
        ]]
    );

    // A second pass finds both current and installed: nothing to resend.
    let current = Arc::new(ScriptedSink {
        current: vec!["ads", "trackers"],
        verified: vec!["ads", "trackers"],
        ..Default::default()
    });
    let mgr = mgr.with_rule_sink(current.clone());
    mgr.reconcile().await;
    assert!(pushed(&current).is_empty());
}

/// Review H1: a list the daemon already holds unchanged (its committed
/// snapshot, its file checked in this run) is reported installed without
/// a push.
#[tokio::test]
async fn a_list_already_current_on_the_daemon_is_installed_without_a_push() {
    let sink = Arc::new(ScriptedSink {
        current: vec!["ads"],
        verified: vec!["ads"],
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile().await;
    assert!(pushed(&sink).is_empty());
    assert!(matches!(
        mgr.enforcement("ads"),
        Enforcement::RuleInstalled { .. }
    ));
}

#[tokio::test]
async fn an_unavailable_daemon_stops_the_pass_without_deleting_anything() {
    let sink = Arc::new(ScriptedSink {
        unavailable: Some("The firewall service didn't answer"),
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads", "trackers"]);
    mgr.reconcile().await;
    assert_eq!(pushed(&sink).len(), 1);
    assert!(sink.orphan_passes.lock().unwrap().is_empty());
    // Not known to be unenforced: "Not confirmed yet", with the reason.
    assert_eq!(
        mgr.enforcement("ads"),
        Enforcement::Unconfirmed {
            reason: "The firewall service didn't answer".into()
        }
    );
}

#[tokio::test]
async fn a_sink_that_installs_nothing_is_never_reconciled_or_asked_to_remove() {
    let mgr = manager(Arc::new(ScriptedSink::default()), &["ads"]).with_rule_sink(Arc::new(
        NoopRuleSink::new("no state directory: in-process"),
    ));
    mgr.reconcile().await;
    assert_eq!(
        mgr.enforcement("ads"),
        Enforcement::NotEnforced {
            reason: "no state directory: in-process".into()
        }
    );
    mgr.remove_subscription("ads").await.unwrap();
}

#[tokio::test]
async fn unsubscribing_removes_the_lists_rules_through_the_sink() {
    let sink = Arc::new(ScriptedSink::default());
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.remove_subscription("ads").await.unwrap();
    assert_eq!(*sink.removed.lock().unwrap(), vec!["ads".to_string()]);
    assert!(mgr.subscription("ads").is_none());
}

fn subscription(id: &str, entry_count: i64) -> Subscription {
    Subscription {
        id: id.into(),
        url: format!("https://example.invalid/{id}"),
        display_name: id.into(),
        format_hint: None,
        refresh_interval_secs: 86_400,
        last_fetched_at: Some(Utc::now()),
        last_attempt_at: None,
        last_fetch_status: FetchStatus::Ok,
        entry_count,
    }
}

/// Review M2: a store that can't be read must not look like "no
/// subscriptions": reconcile would then delete every blocklist rule and
/// file. Nothing is deleted, and the page says why.
#[tokio::test]
async fn an_unreadable_store_deletes_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blocklists.sqlite3");
    BlocklistStore::open(&path)
        .unwrap()
        .upsert_subscription(&subscription("ads", 2))
        .unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE subscriptions SET refresh_interval_secs = 'garbled'",
            [],
        )
        .unwrap();
    let sink = Arc::new(ScriptedSink::default());
    let mgr = BlocklistsManager::new(Arc::new(BlocklistStore::open(&path).unwrap()))
        .with_rule_sink(sink.clone())
        .with_storage_status(StorageStatus {
            unreadable: false,
            persistent: true,
            reason: None,
        });
    mgr.reconcile().await;
    assert!(
        sink.orphan_passes.lock().unwrap().is_empty(),
        "rules were purged"
    );
    // Its own state: still the persistent store, but unreadable.
    let status = mgr.storage_status();
    assert!(status.unreadable && status.persistent, "{status:?}");
    assert!(
        status
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("Couldn't read the saved blocklists")),
        "{status:?}"
    );

    // Re-review N3: nor is anything new installed (the size limit can't
    // count the rules already there).
    let mgr = mgr.with_fetcher(Arc::new(super::test_helpers::FixtureFetcher::default()));
    let id = mgr
        .add_subscription(&super::test_helpers::fixture_url("domains-tiny.txt"))
        .await
        .unwrap();
    assert_eq!(mgr.refresh_now(&id).await.unwrap(), FetchStatus::Ok);
    mgr.reconcile().await;
    assert!(pushed(&sink).is_empty());
    assert_eq!(
        mgr.enforcement(&id),
        Enforcement::NotEnforced {
            reason: UNREADABLE_STORE_REASON.into()
        }
    );
}

/// Review M1: opensnitchd keeps every list in memory and fails open if it
/// is killed, so lists past [`AGGREGATE_MAX_HOSTS`] in total, in the order
/// they were subscribed, get no files and no rule.
#[tokio::test]
async fn lists_past_the_total_size_limit_get_no_rule_in_subscription_order() {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    // Subscription (insertion) order differs from id order on purpose.
    for (id, count) in [
        ("zz-first", 1_500_000),
        ("aa-second", 600_000),
        ("mm-third", 400_000),
    ] {
        store.upsert_subscription(&subscription(id, count)).unwrap();
        store
            .replace_entries(id, &["a.example", "b.example"])
            .unwrap();
        // `replace_entries` counts the two rows; the size is what matters.
        store.upsert_subscription(&subscription(id, count)).unwrap();
    }
    let sink = Arc::new(ScriptedSink::default());
    let mgr = BlocklistsManager::new(store).with_rule_sink(sink.clone());
    mgr.reconcile().await;
    assert_eq!(pushed(&sink), vec![("zz-first".to_string(), 2)]);
    let mut removed = sink.removed.lock().unwrap().clone();
    removed.sort();
    assert_eq!(removed, vec!["aa-second", "mm-third"]);
    for (id, total) in [("aa-second", "2,100,000"), ("mm-third", "2,500,000")] {
        match mgr.enforcement(id) {
            Enforcement::NotEnforced { reason } => {
                assert!(
                    reason.starts_with(OVER_LIMIT_REASON_PREFIX),
                    "{id}: {reason}"
                );
                assert!(
                    reason.contains(total) && reason.contains("2,000,000"),
                    "{reason}"
                );
            }
            other => panic!("{id}: {other:?}"),
        }
    }
    assert_eq!(AGGREGATE_MAX_HOSTS, 2_000_000);
}

/// The same limit on a download: a list that doesn't fit after the lists
/// subscribed before it is never pushed.
#[tokio::test]
async fn a_download_past_the_total_size_limit_is_not_pushed() {
    use super::test_helpers::{fixture_url, FixtureFetcher};
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    store
        .upsert_subscription(&subscription("big", AGGREGATE_MAX_HOSTS as i64))
        .unwrap();
    let mut tiny = subscription("tiny", 0);
    tiny.url = fixture_url("domains-tiny.txt");
    tiny.last_fetched_at = None;
    store.upsert_subscription(&tiny).unwrap();
    let sink = Arc::new(ScriptedSink::default());
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    assert_eq!(mgr.refresh_now("tiny").await.unwrap(), FetchStatus::Ok);
    assert!(pushed(&sink).is_empty());
    assert!(matches!(
        mgr.enforcement("tiny"),
        Enforcement::NotEnforced { reason } if reason.starts_with(OVER_LIMIT_REASON_PREFIX)
    ));
}

/// Review (code M): a list the daemon refused is retried by a full
/// reconcile (a new daemon connection, a refresh tick), not by the
/// clean-up after a subscription change, so one subscribe sends one try.
#[tokio::test]
async fn only_a_full_reconcile_retries_a_refused_list() {
    let sink = Arc::new(ScriptedSink {
        refuse: Some("lists operators are not accepted from the UI"),
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile_with(ReconcileScope::Full).await;
    assert_eq!(pushed(&sink).len(), 1);
    mgr.reconcile_with(ReconcileScope::CleanUp).await;
    assert_eq!(
        pushed(&sink).len(),
        1,
        "the clean-up retried a refused list"
    );
    mgr.reconcile_with(ReconcileScope::Full).await;
    assert_eq!(pushed(&sink).len(), 2);
}

/// An unchanged download still reaches the sink, so a refused or timed-out
/// install is retried on every refresh.
#[tokio::test]
async fn every_refresh_reaches_the_sink_even_when_unchanged() {
    use super::test_helpers::{fixture_url, FixtureFetcher};
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mut tiny = subscription("tiny", 0);
    tiny.url = fixture_url("domains-tiny.txt");
    store.upsert_subscription(&tiny).unwrap();
    let sink = Arc::new(ScriptedSink::default());
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    mgr.refresh_now("tiny").await.unwrap();
    mgr.refresh_now("tiny").await.unwrap();
    assert_eq!(pushed(&sink).len(), 2);
}

/// Re-review N1: the first full pass of a run re-checks the files of lists
/// already in place against the store (the sink rewrites only a file that
/// differs and resends nothing in place).
#[tokio::test]
async fn the_first_full_pass_checks_the_files_of_lists_already_in_place() {
    let sink = Arc::new(ScriptedSink {
        current: vec!["ads"],
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile().await;
    assert_eq!(pushed(&sink), vec![("ads".to_string(), 2)]);
    assert!(matches!(
        mgr.enforcement("ads"),
        Enforcement::RuleInstalled { .. }
    ));
}

/// Code re-review: once a list's files were checked in this run, a retry
/// resends only its rule, without reading its rows again.
#[tokio::test]
async fn a_list_with_checked_files_only_has_its_rule_resent() {
    let sink = Arc::new(ScriptedSink {
        verified: vec!["ads"],
        ..Default::default()
    });
    let mgr = manager(sink.clone(), &["ads"]);
    mgr.reconcile().await;
    assert!(pushed(&sink).is_empty());
    assert_eq!(*sink.reinstalls.lock().unwrap(), vec!["ads".to_string()]);
}

/// Code re-review: the size limit is checked before a list's rows are
/// read: an over-limit list whose rows can't even be read says it is over
/// the limit.
#[tokio::test]
async fn the_size_limit_is_checked_before_reading_a_lists_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blocklists.sqlite3");
    let store = BlocklistStore::open(&path).unwrap();
    for (id, count) in [("first", AGGREGATE_MAX_HOSTS as i64), ("second", 2)] {
        store.upsert_subscription(&subscription(id, count)).unwrap();
        store
            .replace_entries(id, &["a.example", "b.example"])
            .unwrap();
        store.upsert_subscription(&subscription(id, count)).unwrap();
    }
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE entries SET host = X'FF' \
             WHERE subscription_id = 'second' AND host = 'a.example'",
            [],
        )
        .unwrap();
    let sink = Arc::new(ScriptedSink::default());
    let mgr = BlocklistsManager::new(Arc::new(store)).with_rule_sink(sink.clone());
    mgr.reconcile().await;
    match mgr.enforcement("second") {
        Enforcement::NotEnforced { reason } => {
            assert!(reason.starts_with(OVER_LIMIT_REASON_PREFIX), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}
