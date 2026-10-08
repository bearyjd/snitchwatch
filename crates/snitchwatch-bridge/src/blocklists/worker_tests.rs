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

fn manager_with(fetcher: Arc<dyn BlocklistFetch>) -> Arc<BlocklistsManager> {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
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
    let mgr = manager_with(fetcher.clone());
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
    assert_eq!(mgr.store().list_subscriptions().unwrap().len(), 2);
}

#[tokio::test]
async fn try_route_queues_blocklist_messages_and_returns_the_rest() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()));
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
    wait_for("the subscription", || {
        mgr.store().list_subscriptions().unwrap().len() == 1
    })
    .await;
    let id = mgr.store().list_subscriptions().unwrap()[0].id.clone();
    assert_eq!(
        worker.try_route(ClientMessage::UnsubscribeBlocklist { id }),
        None
    );
    wait_for("the unsubscribe", || {
        mgr.store().list_subscriptions().unwrap().is_empty()
    })
    .await;
    handle.abort();
}

/// A full queue refuses the subscribe visibly instead of blocking the caller.
#[tokio::test]
async fn a_full_queue_rejects_a_subscribe_without_blocking() {
    let mgr = manager_with(Arc::new(FixtureFetcher::default()));
    // No worker task: nothing drains the queue.
    let (tx, _rx) = mpsc::channel(1);
    let worker = BlocklistWorker {
        tx,
        mgr: mgr.clone(),
    };
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

/// The refresh loop only enqueues `RefreshDue`; the worker fetches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_loop_drives_pending_subscriptions_through_the_worker() {
    let fetcher = Arc::new(FixtureFetcher::default());
    let mgr = manager_with(fetcher.clone());
    mgr.store()
        .upsert_subscription(&Subscription {
            id: "tiny".into(),
            url: fixture_url("domains-tiny.txt"),
            display_name: "tiny".into(),
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        })
        .unwrap();
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
