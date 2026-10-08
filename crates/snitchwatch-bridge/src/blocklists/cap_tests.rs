//! Issue #73: a list that grows on refresh must not leave the daemon holding
//! more than the total size limit, even briefly. Later lists the new total
//! pushes past the limit are taken off the daemon *before* the grown list's
//! files are written.

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;

use super::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;

/// Logs what it is asked, in order.
#[derive(Default)]
struct Log(StdMutex<Vec<String>>);

#[async_trait]
impl RuleSink for Log {
    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        self.0
            .lock()
            .unwrap()
            .push(format!("replace:{list_id}:{}", hosts.len()));
        Ok(())
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.0.lock().unwrap().push(format!("remove:{list_id}"));
        Ok(())
    }
}

/// Serves `hosts` hosts for every URL.
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

const CAP: u64 = 10;

/// `first`, `second`, `third` in that order, 3 hosts each: 9 of the 10.
fn manager(grown_to: usize) -> (BlocklistsManager, Arc<Log>) {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for id in ["first", "second", "third"] {
        store
            .upsert_subscription(&Subscription {
                id: id.into(),
                url: format!("https://example.invalid/{id}"),
                display_name: id.into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: Some(Utc::now()),
                last_attempt_at: None,
                last_fetch_status: FetchStatus::Ok,
                entry_count: 3,
            })
            .unwrap();
        store
            .replace_entries(id, &["a.example", "b.example", "c.example"])
            .unwrap();
    }
    let log = Arc::new(Log::default());
    let mgr = BlocklistsManager::new(store)
        .with_rule_sink(log.clone())
        .with_fetcher(Arc::new(Fetcher(grown_to)))
        .with_aggregate_cap(CAP);
    (mgr, log)
}

fn log(log: &Log) -> Vec<String> {
    log.0.lock().unwrap().clone()
}

fn over_limit(mgr: &BlocklistsManager, id: &str) -> bool {
    matches!(mgr.enforcement(id),
        Enforcement::NotEnforced { reason } if reason.starts_with(OVER_LIMIT_REASON_PREFIX))
}

#[tokio::test]
async fn lists_the_new_total_pushes_past_the_limit_come_off_before_the_grown_list_goes_on() {
    let (mgr, sink) = manager(6);
    let mut events = mgr.subscribe();
    mgr.refresh_now("first").await.unwrap();
    // 6 + 3 + 3 = 12 > 10: `third` (cumulative 12) is over, `second` (9) is not.
    assert_eq!(log(&sink), vec!["remove:third", "replace:first:6"]);
    assert!(over_limit(&mgr, "third"));
    assert!(!over_limit(&mgr, "second"));
    let mut told = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let BlocklistEvent::StatusChanged { subscription_id } = event {
            told.push(subscription_id);
        }
    }
    assert!(told.contains(&"third".to_string()), "{told:?}");
}

#[tokio::test]
async fn a_grown_list_over_the_limit_on_its_own_takes_the_later_ones_off_too() {
    let (mgr, sink) = manager(11);
    mgr.refresh_now("first").await.unwrap();
    assert_eq!(
        log(&sink),
        vec!["remove:second", "remove:third", "remove:first"],
        "nothing is installed past the limit, and nothing was installed first"
    );
    for id in ["first", "second", "third"] {
        assert!(over_limit(&mgr, id), "{id}");
    }
}

#[tokio::test]
async fn a_refresh_that_stays_under_the_limit_removes_nothing() {
    let (mgr, sink) = manager(4);
    mgr.refresh_now("first").await.unwrap();
    assert_eq!(log(&sink), vec!["replace:first:4"]);
}

#[tokio::test]
async fn a_list_already_demoted_is_not_removed_again_by_every_refresh() {
    let (mgr, sink) = manager(6);
    mgr.refresh_now("first").await.unwrap();
    mgr.refresh_now("first").await.unwrap();
    let removals = log(&sink)
        .into_iter()
        .filter(|entry| entry == "remove:third")
        .count();
    assert_eq!(removals, 1);
}
