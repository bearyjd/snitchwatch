//! Tests for the single blocklist worker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;

use tokio::sync::Notify;

use super::*;
use crate::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};
use crate::blocklists::test_helpers::{fixture_url, FixtureFetcher};
use crate::blocklists::BlocklistEvent;

const SLOW_URL: &str = "https://slow.invalid/first.txt";
const FAST_URL: &str = "https://fast.invalid/second.txt";

/// Holds every fetch of [`SLOW_URL`] until `gate` is notified; records the
/// order fetches finish in and the most fetches ever in flight at once.
#[derive(Default)]
struct GatedFetcher {
    gate: Notify,
    started: Notify,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    finished: StdMutex<Vec<String>>,
}

#[async_trait::async_trait]
impl BlocklistFetch for GatedFetcher {
    async fn fetch(&self, url: &str) -> FetchOutcome {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        if url == SLOW_URL {
            self.started.notify_one();
            self.gate.notified().await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.finished.lock().unwrap().push(url.to_string());
        process_body("0.0.0.0 ads.example\n")
    }
}

fn subscription(id: &str, url: &str) -> Subscription {
    Subscription {
        id: id.into(),
        url: url.into(),
        display_name: id.into(),
        format_hint: None,
        refresh_interval_secs: 86_400,
        last_fetched_at: None,
        last_attempt_at: None,
        last_fetch_status: FetchStatus::Pending,
        entry_count: 0,
    }
}

/// A manager over a store already holding `subs`.
fn manager_with(fetcher: Arc<dyn BlocklistFetch>, subs: &[Subscription]) -> Arc<BlocklistsManager> {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for sub in subs {
        store.upsert_subscription(sub).unwrap();
    }
    Arc::new(BlocklistsManager::new(store).with_fetcher(fetcher))
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Two subscribes: the first fetch blocks, the second would be instant. One
/// worker must still finish them in order, one at a time. A spawn-per-job
/// worker finishes the second first and runs both at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jobs_run_in_order_with_one_fetch_at_a_time() {
    let fetcher = Arc::new(GatedFetcher::default());
    let mgr = manager_with(fetcher.clone(), &[]);
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());

    assert!(worker.enqueue(BlocklistJob::Subscribe {
        url: SLOW_URL.into()
    }));
    assert!(worker.enqueue(BlocklistJob::Subscribe {
        url: FAST_URL.into()
    }));
    fetcher.started.notified().await;
    // Give a concurrent worker every chance to start the second fetch.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        fetcher.finished.lock().unwrap().is_empty(),
        "a job ran while the first fetch was still in flight"
    );
    fetcher.gate.notify_one();
    wait_for("both fetches", || {
        fetcher.finished.lock().unwrap().len() == 2
    })
    .await;
    handle.abort();

    assert_eq!(
        *fetcher.finished.lock().unwrap(),
        vec![SLOW_URL.to_string(), FAST_URL.to_string()]
    );
    assert_eq!(fetcher.max_in_flight.load(Ordering::SeqCst), 1);
    assert_eq!(mgr.subscriptions().len(), 2);
}

/// S3: scheduled refreshes don't hold the queue. A subscribe queued while
/// one scheduled download runs goes before the next scheduled one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscribe_goes_between_scheduled_refreshes() {
    const OTHER_DUE: &str = "https://z-due.invalid/other.txt";
    const NEW: &str = "https://new.invalid/new.txt";
    let fetcher = Arc::new(GatedFetcher::default());
    let mgr = manager_with(
        fetcher.clone(),
        &[subscription("a", SLOW_URL), subscription("b", OTHER_DUE)],
    );
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());
    assert!(worker.enqueue(BlocklistJob::RefreshDue));
    fetcher.started.notified().await;
    assert!(worker.enqueue(BlocklistJob::Subscribe { url: NEW.into() }));
    fetcher.gate.notify_one();
    wait_for("three fetches", || {
        fetcher.finished.lock().unwrap().len() == 3
    })
    .await;
    handle.abort();
    assert_eq!(
        *fetcher.finished.lock().unwrap(),
        vec![SLOW_URL.to_string(), NEW.to_string(), OTHER_DUE.to_string()]
    );
}

#[tokio::test]
async fn try_route_queues_blocklist_messages_and_returns_the_rest() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()), &[]);
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());
    assert_eq!(
        worker.try_route(ClientMessage::Undo),
        Some(ClientMessage::Undo)
    );
    assert_eq!(
        worker.try_route(ClientMessage::SubscribeBlocklist {
            url: fixture_url("domains-tiny.txt")
        }),
        None
    );
    wait_for("the subscription", || mgr.subscriptions().len() == 1).await;
    let id = mgr.subscriptions()[0].id.clone();
    assert_eq!(
        worker.try_route(ClientMessage::UnsubscribeBlocklist { id }),
        None
    );
    wait_for("the unsubscribe", || mgr.subscriptions().is_empty()).await;
    handle.abort();
}

/// A worker handle whose queue nothing drains, so tests can fill it.
fn undrained(
    mgr: &Arc<BlocklistsManager>,
    capacity: usize,
) -> (BlocklistWorker, mpsc::Receiver<BlocklistJob>) {
    let (tx, rx) = mpsc::channel(capacity);
    let worker = BlocklistWorker {
        tx,
        mgr: mgr.clone(),
        unsubscribes: Default::default(),
    };
    (worker, rx)
}

/// A full queue refuses the subscribe visibly instead of blocking the caller.
#[tokio::test]
async fn a_full_queue_rejects_a_subscribe_without_blocking() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()), &[]);
    let (worker, _rx) = undrained(&mgr, 1);
    let mut events = mgr.subscribe();
    assert!(worker.enqueue(BlocklistJob::RefreshDue));
    assert!(!worker.enqueue(BlocklistJob::Subscribe {
        url: fixture_url("domains-tiny.txt")
    }));
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::SubscriptionRejected { .. })
    ));
}

/// S5: a bad URL is refused before it takes a queue slot.
#[tokio::test]
async fn a_bad_url_is_refused_before_queueing() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()), &[]);
    let (worker, mut rx) = undrained(&mgr, 4);
    let mut events = mgr.subscribe();
    let long = format!("https://x.example/{}", "a".repeat(10_000));
    for url in ["http://x.example/hosts", long.as_str()] {
        assert_eq!(
            worker.try_route(ClientMessage::SubscribeBlocklist { url: url.into() }),
            None
        );
        match events.try_recv() {
            Ok(BlocklistEvent::SubscriptionRejected { url, .. }) => {
                assert!(url.len() <= crate::blocklists::fetcher::MAX_URL_LEN)
            }
            other => panic!("expected SubscriptionRejected, got {other:?}"),
        }
    }
    assert!(rx.try_recv().is_err(), "a refused URL was queued");
}

/// S3: an unsubscribe is never dropped, even with the job queue full, and
/// runs before the queued jobs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsubscribe_is_never_dropped_and_goes_first() {
    let fetcher = Arc::new(GatedFetcher::default());
    let mgr = manager_with(fetcher.clone(), &[subscription("keep-out", FAST_URL)]);
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());
    // Block the worker on a slow subscribe, then fill the queue with
    // scheduled refreshes (which would download the due "keep-out" list).
    assert!(worker.enqueue(BlocklistJob::Subscribe {
        url: SLOW_URL.into()
    }));
    fetcher.started.notified().await;
    while worker.enqueue(BlocklistJob::RefreshDue) {}
    worker.unsubscribe("keep-out".into());
    fetcher.gate.notify_one();
    wait_for("the unsubscribe", || !mgr.has_subscription("keep-out")).await;
    // Let the queued refreshes drain: none of them may find "keep-out".
    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.abort();
    assert_eq!(
        *fetcher.finished.lock().unwrap(),
        vec![SLOW_URL.to_string()]
    );
}

/// Unsubscribing an id that isn't stored (a refused row) just resends the
/// list, so the row disappears.
#[tokio::test]
async fn unsubscribing_an_unknown_id_resends_the_list() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()), &[]);
    let (worker, _rx) = undrained(&mgr, 1);
    let mut events = mgr.subscribe();
    worker.unsubscribe("never-stored".into());
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::SubscriptionsChanged)
    ));
    assert!(worker.unsubscribes.pop().is_none());
}

/// The refresh loop only enqueues `RefreshDue`; the worker fetches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_loop_drives_pending_subscriptions_through_the_worker() {
    let fetcher = Arc::new(FixtureFetcher::default());
    let mgr = manager_with(
        fetcher.clone(),
        &[subscription("tiny", &fixture_url("domains-tiny.txt"))],
    );
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());
    let refresh = worker.spawn_refresh_loop(Duration::from_millis(50));
    wait_for("the scheduled refresh", || {
        mgr.store()
            .list_entries("tiny")
            .unwrap()
            .contains(&"doubleclick.net".to_string())
    })
    .await;
    // Fetched once, then not again: it is no longer due.
    tokio::time::sleep(Duration::from_millis(300)).await;
    refresh.abort();
    handle.abort();
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn aborting_the_tasks_stops_them() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()), &[]);
    let (tx, _rx) = broadcast::channel(4);
    let (_synced_tx, synced) = tokio::sync::watch::channel(0);
    let tasks = BlocklistTasks::spawn(mgr, tx, Duration::from_secs(3600), Some(synced));
    tasks.abort();
    wait_for("the aborted tasks", || tasks.is_finished()).await;
}

/// Only a known list's id reaches the event bus (and the logs).
#[tokio::test]
async fn entry_requests_for_unknown_ids_are_dropped() {
    let mgr = manager_with(
        Arc::new(FixtureFetcher::default()),
        &[subscription("known", FAST_URL)],
    );
    let (worker, _rx) = undrained(&mgr, 1);
    let mut events = mgr.subscribe();
    let request = |id: &str| ClientMessage::RequestBlocklistEntries {
        subscription_id: id.to_string(),
        offset: 0,
        limit: None,
    };
    assert_eq!(
        worker.try_route(request("forged\nWARN fake log line")),
        None
    );
    assert!(events.try_recv().is_err(), "an unknown id reached the bus");
    assert_eq!(worker.try_route(request("known")), None);
    assert!(matches!(
        events.try_recv(),
        Ok(BlocklistEvent::EntriesRequested { subscription_id, .. }) if subscription_id == "known"
    ));
}

/// Counts reconcile passes (each ends in `remove_orphans`).
#[derive(Default)]
struct ReconcileCounter {
    passes: AtomicUsize,
}

#[async_trait::async_trait]
impl crate::blocklists::RuleSink for ReconcileCounter {
    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _hosts: Vec<String>,
    ) -> Result<(), crate::blocklists::NotInstalled> {
        Ok(())
    }

    async fn remove_orphans(&self, _keep: &[String]) {
        self.passes.fetch_add(1, Ordering::SeqCst);
    }
}

fn counted_manager() -> (Arc<BlocklistsManager>, Arc<ReconcileCounter>) {
    let sink = Arc::new(ReconcileCounter::default());
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    (Arc::new(mgr), sink)
}

/// Issue #45 PR B: each committed daemon rules snapshot (a new daemon
/// connection) reconciles the lists, on the worker.
#[tokio::test]
async fn each_committed_rules_snapshot_triggers_a_reconcile() {
    let (mgr, sink) = counted_manager();
    let (tx, _rx) = broadcast::channel(16);
    let (synced_tx, synced) = tokio::sync::watch::channel(0u64);
    let tasks = BlocklistTasks::spawn(mgr, tx, Duration::from_secs(3600), Some(synced));
    // The refresh loop's first tick (at start) reconciles once.
    wait_for("the startup tick's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(sink.passes.load(Ordering::SeqCst), 1, "no snapshot yet");
    synced_tx.send_modify(|g| *g += 1);
    wait_for("the first snapshot's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 2
    })
    .await;
    synced_tx.send_modify(|g| *g += 1);
    wait_for("the second snapshot's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 3
    })
    .await;
    tasks.abort();
}

/// Subscription changes reconcile too, after the subscribe or unsubscribe.
#[tokio::test]
async fn subscribing_and_unsubscribing_trigger_a_reconcile() {
    let (mgr, sink) = counted_manager();
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());
    assert_eq!(
        worker.try_route(ClientMessage::SubscribeBlocklist {
            url: fixture_url("domains-tiny.txt"),
        }),
        None
    );
    wait_for("the subscribe's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 1
    })
    .await;
    let id = mgr.subscriptions()[0].id.clone();
    worker.unsubscribe(id);
    wait_for("the unsubscribe's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 2
    })
    .await;
    assert!(mgr.subscriptions().is_empty());
    handle.abort();
}

/// Review (code M): a scheduled refresh tick also reconciles, so a list the
/// daemon refused or didn't answer for is retried without a restart.
#[tokio::test]
async fn a_refresh_tick_triggers_a_full_reconcile() {
    let (mgr, sink) = counted_manager();
    let (worker, handle) = BlocklistWorker::spawn(mgr);
    assert!(worker.enqueue(BlocklistJob::RefreshDue));
    wait_for("the refresh tick's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 1
    })
    .await;
    handle.abort();
}

/// Refuses every install; counts installs and reconcile passes.
#[derive(Default)]
struct RefusingCounter {
    installs: AtomicUsize,
    passes: AtomicUsize,
}

#[async_trait::async_trait]
impl crate::blocklists::RuleSink for RefusingCounter {
    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _hosts: Vec<String>,
    ) -> Result<(), crate::blocklists::NotInstalled> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        Err(crate::blocklists::NotInstalled::new("refused"))
    }

    async fn remove_orphans(&self, _keep: &[String]) {
        self.passes.fetch_add(1, Ordering::SeqCst);
    }
}

fn manager_over(
    sink: Arc<dyn crate::blocklists::RuleSink>,
    subs: &[Subscription],
) -> Arc<BlocklistsManager> {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for sub in subs {
        store.upsert_subscription(sub).unwrap();
    }
    Arc::new(
        BlocklistsManager::new(store)
            .with_fetcher(Arc::new(FixtureFetcher::default()))
            .with_rule_sink(sink),
    )
}

/// Code re-review: one refresh tick, however many lists are due, runs one
/// full reconcile, after its downloads.
#[tokio::test]
async fn a_refresh_tick_with_several_due_lists_reconciles_once() {
    let sink = Arc::new(ReconcileCounter::default());
    let url = fixture_url("domains-tiny.txt");
    let mgr = manager_over(
        sink.clone(),
        &[
            subscription("a", &url),
            subscription("b", &url),
            subscription("c", &url),
        ],
    );
    let (worker, handle) = BlocklistWorker::spawn(mgr.clone());
    assert!(worker.enqueue(BlocklistJob::RefreshDue));
    wait_for("all three downloads", || {
        mgr.subscriptions()
            .iter()
            .all(|s| s.last_fetch_status == FetchStatus::Ok)
    })
    .await;
    wait_for("the tick's reconcile", || {
        sink.passes.load(Ordering::SeqCst) >= 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        sink.passes.load(Ordering::SeqCst),
        1,
        "one reconcile per tick"
    );
    handle.abort();
}

/// Code re-review: a list refused while it was downloaded in a refresh
/// tick isn't tried again by the same tick's reconcile.
#[tokio::test]
async fn a_list_refused_during_a_refresh_tick_is_tried_once_in_it() {
    let sink = Arc::new(RefusingCounter::default());
    let mgr = manager_over(
        sink.clone(),
        &[subscription("a", &fixture_url("domains-tiny.txt"))],
    );
    let (worker, handle) = BlocklistWorker::spawn(mgr);
    assert!(worker.enqueue(BlocklistJob::RefreshDue));
    wait_for("the tick's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(sink.installs.load(Ordering::SeqCst), 1);
    handle.abort();
}

/// Code re-review: an unsubscribe can free room under the total size
/// limit, so it runs a full reconcile (which also retries refused lists).
#[tokio::test]
async fn an_unsubscribe_runs_a_full_reconcile() {
    let sink = Arc::new(RefusingCounter::default());
    let downloaded = |id: &str| Subscription {
        last_fetched_at: Some(chrono::Utc::now()),
        last_fetch_status: FetchStatus::Ok,
        entry_count: 1,
        ..subscription(id, &format!("https://example.invalid/{id}"))
    };
    let mgr = manager_over(sink.clone(), &[downloaded("a"), downloaded("b")]);
    mgr.reconcile().await;
    assert_eq!(sink.installs.load(Ordering::SeqCst), 2);
    let (worker, handle) = BlocklistWorker::spawn(mgr);
    worker.unsubscribe("b".to_string());
    wait_for("the unsubscribe's reconcile", || {
        sink.passes.load(Ordering::SeqCst) == 2
    })
    .await;
    assert_eq!(
        sink.installs.load(Ordering::SeqCst),
        3,
        "the refused list wasn't retried"
    );
    handle.abort();
}
