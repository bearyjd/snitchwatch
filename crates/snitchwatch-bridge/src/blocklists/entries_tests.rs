//! Issue #67: how much a bridge keeps on disk for its lists, and the pages
//! it serves back from there.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use super::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;
use crate::translator::downstream::build_blocklist_entries_page;
use crate::ws_messages::ServerMessage;

/// Serves `hosts` distinct hosts for every URL, counting its downloads.
struct Fetcher {
    hosts: usize,
    calls: AtomicUsize,
}

impl Fetcher {
    fn new(hosts: usize) -> Arc<Self> {
        Arc::new(Self {
            hosts,
            calls: AtomicUsize::new(0),
        })
    }

    fn downloads(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BlocklistFetch for Fetcher {
    async fn fetch(&self, _url: &str) -> FetchOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body: String = (0..self.hosts)
            .map(|i| format!("0.0.0.0 h{i}.example\n"))
            .collect();
        process_body(&body)
    }
}

const STORED_CAP: u64 = 10;

fn subscription(id: &str, held: usize) -> Subscription {
    Subscription {
        id: id.into(),
        url: format!("https://example.invalid/{id}"),
        display_name: id.into(),
        format_hint: None,
        refresh_interval_secs: 86_400,
        last_fetched_at: (held > 0).then(Utc::now),
        last_attempt_at: None,
        last_fetch_status: if held > 0 {
            FetchStatus::Ok
        } else {
            FetchStatus::Pending
        },
        entry_count: held as i64,
    }
}

/// Lists subscribed in this order, each holding the given number of hosts,
/// over a fetcher serving `serves` hosts. The saved-hosts limit is 10.
fn manager_of(held: &[(&str, usize)], serves: usize) -> (BlocklistsManager, Arc<Fetcher>) {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for (id, hosts) in held {
        store
            .upsert_subscription(&subscription(id, *hosts))
            .unwrap();
        let old: Vec<String> = (0..*hosts)
            .map(|i| format!("old{i}.{id}.example"))
            .collect();
        let refs: Vec<&str> = old.iter().map(String::as_str).collect();
        store.replace_entries(id, &refs).unwrap();
    }
    let fetcher = Fetcher::new(serves);
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(fetcher.clone())
        .with_stored_cap(STORED_CAP);
    (mgr, fetcher)
}

/// `a` and `b` hold 3 hosts each; `c` was never downloaded.
fn manager(serves: usize) -> BlocklistsManager {
    manager_of(&[("a", 3), ("b", 3), ("c", 0)], serves).0
}

fn failed_reason(status: FetchStatus) -> String {
    match status {
        FetchStatus::Failed { reason } => reason,
        other => panic!("expected a failed download, got {other:?}"),
    }
}

#[tokio::test]
async fn a_download_that_would_pass_the_saved_hosts_limit_is_refused_and_not_stored() {
    let mgr = manager(5); // the lists before `c` hold 6 of the 10
    let reason = failed_reason(mgr.refresh_now("c").await.unwrap());
    assert!(
        reason.contains("10 hosts") && reason.contains("all lists"),
        "{reason}"
    );
    assert_eq!(mgr.subscription("c").unwrap().entry_count, 0);
    let page = mgr.entries_page("c", 0, 100).await.unwrap();
    assert!(
        page.hosts.is_empty(),
        "nothing of the refused list is saved"
    );
    assert_eq!(page.total, 0);
}

#[tokio::test]
async fn a_download_that_exactly_fits_is_stored() {
    let mgr = manager(4); // 6 + 4 = 10
    assert_eq!(mgr.refresh_now("c").await.unwrap(), FetchStatus::Ok);
    assert_eq!(mgr.subscription("c").unwrap().entry_count, 4);
}

/// Like the total size limit, the earliest lists get the budget: only the
/// lists subscribed before one count against it, so lists after it, however
/// big, can't stop it refreshing.
#[tokio::test]
async fn a_refresh_is_measured_against_the_lists_before_it_only() {
    let mgr = manager(10);
    assert_eq!(mgr.refresh_now("a").await.unwrap(), FetchStatus::Ok);
    assert_eq!(mgr.subscription("a").unwrap().entry_count, 10);

    let mgr = manager(7); // `b`: 3 before it, so 7 fit
    assert_eq!(mgr.refresh_now("b").await.unwrap(), FetchStatus::Ok);
    assert_eq!(mgr.subscription("b").unwrap().entry_count, 7);
}

/// The refused refresh keeps what the list held (and what is enforced), like
/// any other failed download.
#[tokio::test]
async fn a_refused_refresh_keeps_the_lists_earlier_hosts() {
    let mgr = manager(8); // `b`: 3 before it, so 8 don't fit
    let reason = failed_reason(mgr.refresh_now("b").await.unwrap());
    assert!(reason.contains("all lists"), "{reason}");
    let page = mgr.entries_page("b", 0, 100).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.hosts.len(), 3);
    assert!(page.hosts[0].starts_with("old"));
}

/// A store that was already over the limit before the limit existed: the
/// lists inside it carry on, the one it crosses can't grow, and one wholly
/// past it isn't even downloaded.
#[tokio::test]
async fn a_store_already_over_the_limit_leaves_room_by_subscription_order() {
    let (mgr, fetcher) = manager_of(&[("a", 6), ("b", 6), ("c", 0)], 6);
    assert_eq!(mgr.refresh_now("a").await.unwrap(), FetchStatus::Ok);
    assert_eq!(fetcher.downloads(), 1);

    let reason = failed_reason(mgr.refresh_now("b").await.unwrap());
    assert!(reason.contains("all lists"), "{reason}");
    assert_eq!(mgr.subscription("b").unwrap().entry_count, 6, "kept");
    assert_eq!(fetcher.downloads(), 2, "it had to be read to know its size");

    let reason = failed_reason(mgr.refresh_now("c").await.unwrap());
    assert!(reason.contains("all lists"), "{reason}");
    assert_eq!(fetcher.downloads(), 2, "no room at all: nothing downloaded");
}

/// A sum of sizes can't wrap and let a huge store look empty.
#[tokio::test]
async fn the_sum_of_the_lists_before_is_saturating() {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for (id, held) in [("a", i64::MAX), ("b", i64::MAX), ("c", i64::MAX), ("d", 0)] {
        let mut sub = subscription(id, 1);
        sub.entry_count = held;
        store.upsert_subscription(&sub).unwrap();
    }
    let fetcher = Fetcher::new(1);
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(fetcher.clone())
        .with_stored_cap(STORED_CAP);
    failed_reason(mgr.refresh_now("d").await.unwrap());
    assert_eq!(fetcher.downloads(), 0);
}

fn refused_for_room(hours_ago: i64) -> Subscription {
    Subscription {
        last_attempt_at: Some(Utc::now() - chrono::Duration::hours(hours_ago)),
        last_fetch_status: FetchStatus::Failed {
            reason: format!("{STORAGE_LIMIT_REASON_PREFIX} 10 hosts across all lists"),
        },
        ..subscription("c", 0)
    }
}

fn due_with(subs: Vec<Subscription>) -> (BlocklistsManager, Vec<String>) {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for sub in &subs {
        store.upsert_subscription(sub).unwrap();
    }
    let mgr = BlocklistsManager::new(store);
    let due = mgr.due_subscription_ids();
    (mgr, due)
}

/// A download refused for room is refused again until another list goes, so
/// it is not fetched every hour like a failed one; its normal interval does.
#[test]
fn a_list_refused_for_room_is_not_retried_hourly() {
    let (_, due) = due_with(vec![refused_for_room(2)]);
    assert!(due.is_empty(), "{due:?}");
    let (_, due) = due_with(vec![refused_for_room(25)]);
    assert_eq!(due, vec!["c".to_string()], "its interval has passed");

    let mut other = refused_for_room(2);
    other.last_fetch_status = FetchStatus::Failed {
        reason: "Couldn't download the list".into(),
    };
    let (_, due) = due_with(vec![other]);
    assert_eq!(due, vec!["c".to_string()], "an ordinary failure is retried");
}

#[tokio::test]
async fn removing_a_list_makes_a_refused_one_due_at_once() {
    let (mgr, due) = due_with(vec![subscription("a", 3), refused_for_room(2)]);
    assert!(due.is_empty(), "{due:?}");
    mgr.remove_subscription("a").await.unwrap();
    assert_eq!(mgr.due_subscription_ids(), vec!["c".to_string()]);
    // Trying it ends the wait (it is refused again, or saved).
    mgr.refresh_now("c").await.ok();
    assert!(mgr.due_subscription_ids().is_empty());
}

#[tokio::test]
async fn a_page_names_its_download_and_a_refresh_changes_it() {
    let mgr = manager(2);
    mgr.refresh_now("c").await.unwrap();
    let first = mgr.entries_page("c", 0, 100).await.unwrap();
    let fetched = mgr.subscription("c").unwrap().last_fetched_at.unwrap();
    assert_eq!(first.last_fetched_at, Some(fetched.to_rfc3339()));

    mgr.refresh_now("c").await.unwrap();
    let second = mgr.entries_page("c", 0, 100).await.unwrap();
    assert_ne!(second.last_fetched_at, first.last_fetched_at);
}

#[tokio::test]
async fn an_unknown_list_has_no_page() {
    let mgr = manager(1);
    assert!(mgr.entries_page("nope", 0, 10).await.is_err());
}

/// What the GUIs get: the page echoes the request it answers and names the
/// download it was read from.
#[tokio::test]
async fn the_page_message_echoes_the_request_and_names_the_download() {
    let mgr = manager(2);
    mgr.refresh_now("c").await.unwrap();
    let fetched = mgr.subscription("c").unwrap().last_fetched_at.unwrap();
    let msg = build_blocklist_entries_page(&mgr, "c", 0, 10, Some("gui-7".into()))
        .await
        .unwrap();
    match msg {
        ServerMessage::SetBlocklistEntries {
            request_id,
            last_updated_iso8601,
            total,
            ..
        } => {
            assert_eq!(request_id.as_deref(), Some("gui-7"));
            assert_eq!(last_updated_iso8601, Some(fetched.to_rfc3339()));
            assert_eq!(total, 2);
        }
        other => panic!("expected SetBlocklistEntries, got {other:?}"),
    }
}
