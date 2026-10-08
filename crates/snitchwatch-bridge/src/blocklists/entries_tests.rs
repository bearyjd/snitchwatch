//! Issue #67: how much a bridge keeps on disk for its lists, and the pages
//! it serves back from there.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use super::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;
use crate::translator::downstream::build_blocklist_entries_page;
use crate::ws_messages::ServerMessage;

/// Serves `hosts` distinct hosts for every URL.
struct Fetcher(usize);

#[async_trait]
impl BlocklistFetch for Fetcher {
    async fn fetch(&self, _url: &str) -> FetchOutcome {
        let body: String = (0..self.0)
            .map(|i| format!("0.0.0.0 h{i}.example\n"))
            .collect();
        process_body(&body)
    }
}

const STORED_CAP: u64 = 10;

/// `a` and `b` hold 3 hosts each; `c` was never downloaded.
fn manager(serves: usize) -> BlocklistsManager {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for (id, held) in [("a", 3), ("b", 3), ("c", 0)] {
        store
            .upsert_subscription(&Subscription {
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
                entry_count: held,
            })
            .unwrap();
        let hosts: Vec<String> = (0..held).map(|i| format!("old{i}.{id}.example")).collect();
        let refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
        store.replace_entries(id, &refs).unwrap();
    }
    BlocklistsManager::new(store)
        .with_fetcher(Arc::new(Fetcher(serves)))
        .with_stored_cap(STORED_CAP)
}

fn failed_reason(status: FetchStatus) -> String {
    match status {
        FetchStatus::Failed { reason } => reason,
        other => panic!("expected a failed download, got {other:?}"),
    }
}

#[tokio::test]
async fn a_download_that_would_pass_the_saved_hosts_limit_is_refused_and_not_stored() {
    let mgr = manager(5); // 3 + 3 + 5 = 11 > 10
    let status = mgr.refresh_now("c").await.unwrap();
    let reason = failed_reason(status);
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
    let mgr = manager(4); // 3 + 3 + 4 = 10
    assert_eq!(mgr.refresh_now("c").await.unwrap(), FetchStatus::Ok);
    assert_eq!(mgr.subscription("c").unwrap().entry_count, 4);
}

/// A list's own old entries are replaced, not added to: only the other lists
/// count against it.
#[tokio::test]
async fn a_refresh_is_measured_against_the_other_lists_not_its_own_old_copy() {
    let mgr = manager(7); // 7 + 3 (b) + 0 (c) = 10
    assert_eq!(mgr.refresh_now("a").await.unwrap(), FetchStatus::Ok);
    assert_eq!(mgr.subscription("a").unwrap().entry_count, 7);
}

/// The refused refresh keeps what the list held (and what is enforced), like
/// any other failed download.
#[tokio::test]
async fn a_refused_refresh_keeps_the_lists_earlier_hosts() {
    let mgr = manager(8); // 8 + 3 + 0 = 11 > 10
    let reason = failed_reason(mgr.refresh_now("a").await.unwrap());
    assert!(reason.contains("all lists"), "{reason}");
    let page = mgr.entries_page("a", 0, 100).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.hosts.len(), 3);
    assert!(page.hosts[0].starts_with("old"));
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
