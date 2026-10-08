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

/// Logs what it is asked, in order. While `hung`, deleting rules fails as a
/// daemon that doesn't answer would (and every ask is logged).
#[derive(Default)]
struct Log {
    asked: StdMutex<Vec<String>>,
    hung: std::sync::atomic::AtomicBool,
}

impl Log {
    fn note(&self, entry: String) {
        self.asked.lock().unwrap().push(entry);
    }
}

#[async_trait]
impl RuleSink for Log {
    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        self.note(format!("replace:{list_id}:{}", hosts.len()));
        Ok(())
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.note(format!("remove:{list_id}"));
        if self.hung.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(NotInstalled::daemon_unavailable("didn't answer"));
        }
        Ok(())
    }

    async fn remove_blocklist_files(&self, list_id: &str) -> Result<(), NotInstalled> {
        self.note(format!("files:{list_id}"));
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
    log.asked.lock().unwrap().clone()
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

/// A daemon that doesn't answer the first removal isn't waited on for the
/// rest: their files go, and the grown list isn't installed (it would wait on
/// the same daemon) but reads "not confirmed".
#[tokio::test]
async fn a_daemon_that_does_not_answer_is_asked_once_when_lists_are_taken_off() {
    let (mgr, sink) = manager(8); // 8 + 3 + 3 = 14: second and third are over
    sink.hung.store(true, std::sync::atomic::Ordering::SeqCst);
    mgr.refresh_now("first").await.unwrap();
    assert_eq!(
        log(&sink),
        vec!["remove:second", "files:third"],
        "one wait on the daemon, nothing for the grown list"
    );
    assert!(matches!(
        mgr.enforcement("first"),
        Enforcement::Unconfirmed { .. }
    ));
    for id in ["second", "third"] {
        assert!(
            !matches!(mgr.enforcement(id), Enforcement::RuleInstalled { .. }),
            "{id}"
        );
    }
}

/// The same pass with a daemon that answers asks it about every list.
#[tokio::test]
async fn a_daemon_that_answers_is_asked_about_every_list() {
    let (mgr, sink) = manager(8);
    mgr.refresh_now("first").await.unwrap();
    assert_eq!(
        log(&sink),
        vec!["remove:second", "remove:third", "replace:first:8"]
    );
}
