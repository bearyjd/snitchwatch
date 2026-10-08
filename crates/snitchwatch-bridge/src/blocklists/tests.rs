//! Unit tests for `BlocklistsManager` (split out of `mod.rs` for size).
use std::sync::Arc;

use chrono::Utc;

use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::test_helpers::{fixture_url, FixtureFetcher};
use super::*;
use crate::ws_messages::StorageStatus;

fn manager() -> BlocklistsManager {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    BlocklistsManager::new(store)
}

/// A store holding one never-fetched subscription `id` pointing at `url`.
fn store_with(id: &str, url: &str) -> Arc<BlocklistStore> {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    store
        .upsert_subscription(&Subscription {
            id: id.into(),
            url: url.into(),
            display_name: id.into(),
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_attempt_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        })
        .unwrap();
    store
}

#[test]
fn module_exports_compile() {
    let _ = std::any::type_name::<store::Subscription>();
}

#[tokio::test]
async fn add_subscription_emits_subscriptions_changed_event() {
    let mgr = manager();
    let mut rx = mgr.subscribe();
    let id = mgr
        .add_subscription("https://example.invalid/list.txt")
        .await
        .unwrap();
    assert_eq!(id, derive_id("https://example.invalid/list.txt"));
    assert!(id.starts_with("list-"), "unexpected id {id}");
    let evt = rx.recv().await.expect("event");
    assert!(matches!(evt, BlocklistEvent::SubscriptionsChanged));
}

#[tokio::test]
async fn remove_subscription_clears_store() {
    let mgr = manager();
    let id = mgr
        .add_subscription("https://example.invalid/test.txt")
        .await
        .unwrap();
    mgr.remove_subscription(&id).await.unwrap();
    assert!(mgr.store().get_subscription(&id).unwrap().is_none());
    assert!(!mgr.has_subscription(&id));
}

/// Ids are persisted and (PR B) become rule names and list directories, so
/// the exact format is pinned: `<sanitized stem>-<16 hex of SHA-256(url)>`.
#[test]
fn derive_id_is_the_stem_plus_a_url_hash() {
    assert_eq!(
        derive_id("https://x.example/StevenBlack/hosts"),
        "hosts-0a83af5910482d19"
    );
    assert_eq!(
        derive_id("https://x.example/hosts.txt?branch=main"),
        "hosts-de29c9908c0f02fb"
    );
    assert_eq!(derive_id("https://x.example/"), "list-37714a195c1b7577");
}

#[test]
fn derive_id_distinguishes_urls_with_the_same_file_name() {
    let a = derive_id("https://a.example/hosts");
    let b = derive_id("https://b.example/hosts");
    assert_ne!(a, b, "two …/hosts URLs must not share an id");
    assert_eq!(
        a,
        derive_id("https://a.example/hosts"),
        "ids must be stable"
    );
}

#[test]
fn derive_id_uses_only_safe_characters_and_a_bounded_stem() {
    let long = format!("https://x.example/{}", "a.b%20c".repeat(400));
    for url in [
        "https://x.example/path/with%20space",
        "https://x.example/.hidden",
        long.as_str(),
    ] {
        let id = derive_id(url);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "unsafe id {id:?} for {url}"
        );
        assert!(!id.starts_with('.'), "leading dot in {id:?}");
        assert!(
            id.len() <= 64 + 17,
            "unbounded id length {} for {url}",
            id.len()
        );
    }
    assert_eq!(
        derive_display_name("https://x.example/path/with_space"),
        "with space",
        "the display name must not carry the hash suffix"
    );
}

#[tokio::test]
async fn refresh_pushes_the_downloaded_hosts_to_the_sink() {
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct CapturingSink {
        calls: StdMutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl super::RuleSink for CapturingSink {
        async fn replace_blocklist_rules(
            &self,
            list_id: &str,
            hosts: Vec<String>,
        ) -> Result<(), NotInstalled> {
            self.calls
                .lock()
                .unwrap()
                .push((list_id.to_string(), hosts));
            Ok(())
        }
    }

    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let sink: Arc<CapturingSink> = Arc::new(CapturingSink::default());
    let mgr = BlocklistsManager::new(store.clone())
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    mgr.refresh_now("tiny").await.unwrap();
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "expected one push call");
    assert_eq!(calls[0].0, "tiny");
    assert_eq!(calls[0].1, store.list_entries("tiny").unwrap());
    assert!(!calls[0].1.is_empty(), "domains-tiny.txt should have hosts");
    assert!(
        matches!(mgr.enforcement("tiny"), Enforcement::RuleInstalled { .. }),
        "a sink that accepted the rules reports them installed"
    );
}

#[tokio::test]
async fn failed_refresh_preserves_prior_entries() {
    let store = store_with("preserve", &fixture_url("domains-tiny.txt"));
    let mgr =
        BlocklistsManager::new(store.clone()).with_fetcher(Arc::new(FixtureFetcher::default()));
    mgr.refresh_now("preserve").await.unwrap();
    let count_before = store.list_entries("preserve").unwrap().len();
    assert!(count_before > 0, "priming failed");

    // Now point the URL at a fixture that does not exist and refresh again.
    store
        .upsert_subscription(&Subscription {
            id: "preserve".into(),
            url: fixture_url("does-not-exist.txt"),
            display_name: "preserve".into(),
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: Some(Utc::now() - chrono::Duration::seconds(100_000)),
            last_attempt_at: None,
            last_fetch_status: FetchStatus::Ok,
            entry_count: count_before as i64,
        })
        .unwrap();
    // The manager mirrors the store; restart it to see the edited URL.
    let mgr =
        BlocklistsManager::new(store.clone()).with_fetcher(Arc::new(FixtureFetcher::default()));
    let status = mgr.refresh_now("preserve").await.unwrap();
    match status {
        FetchStatus::Failed { reason } => {
            assert!(
                reason.contains("does-not-exist"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // Critical: entries must STILL be present.
    let entries_after = store.list_entries("preserve").unwrap();
    assert_eq!(
        entries_after.len(),
        count_before,
        "failed fetch must not clear cached entries"
    );
}

/// PR A has no daemon rule sink: a successful download must not be reported
/// as enforced (issue #45). The no-op sink yields `NotEnforced`.
#[tokio::test]
async fn refresh_with_the_noop_sink_reports_not_enforced() {
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr = BlocklistsManager::new(store).with_fetcher(Arc::new(FixtureFetcher::default()));
    let unavailable = Enforcement::NotEnforced {
        reason: NO_RULE_SINK_REASON.to_string(),
    };
    assert_eq!(mgr.enforcement("tiny"), unavailable);
    assert_eq!(mgr.refresh_now("tiny").await.unwrap(), FetchStatus::Ok);
    assert_eq!(
        mgr.enforcement("tiny"),
        Enforcement::NotEnforced {
            reason: NO_RULE_SINK_REASON.to_string()
        }
    );
}

#[tokio::test]
async fn a_failing_sink_reports_not_enforced_with_its_error() {
    struct FailingSink;
    #[async_trait]
    impl super::RuleSink for FailingSink {
        async fn replace_blocklist_rules(
            &self,
            _list_id: &str,
            _hosts: Vec<String>,
        ) -> Result<(), NotInstalled> {
            Err(NotInstalled::new("daemon said no"))
        }
    }
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(Arc::new(FailingSink));
    mgr.refresh_now("tiny").await.unwrap();
    match mgr.enforcement("tiny") {
        Enforcement::NotEnforced { reason } => assert!(reason.contains("daemon said no")),
        other => panic!("expected NotEnforced, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_first_download_is_not_enforced() {
    let store = store_with("gone", &fixture_url("does-not-exist.txt"));
    let mgr = BlocklistsManager::new(store).with_fetcher(Arc::new(FixtureFetcher::default()));
    assert!(matches!(
        mgr.refresh_now("gone").await.unwrap(),
        FetchStatus::Failed { .. }
    ));
    assert!(matches!(
        mgr.enforcement("gone"),
        Enforcement::NotEnforced { .. }
    ));
}

#[tokio::test]
async fn unsubscribing_forgets_the_enforcement_state() {
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(Arc::new(CountingSink::installing()));
    mgr.refresh_now("tiny").await.unwrap();
    assert!(matches!(
        mgr.enforcement("tiny"),
        Enforcement::RuleInstalled { .. }
    ));
    mgr.remove_subscription("tiny").await.unwrap();
    assert_eq!(mgr.enforcement("tiny"), Enforcement::Pending);
}

/// A bad URL is never stored, whichever path tries to store it.
#[tokio::test]
async fn add_subscription_refuses_non_https_urls_and_stores_nothing() {
    let mgr = manager();
    for url in [
        "http://x.example/hosts",
        "file:///dev/zero",
        "ftp://x.example/hosts",
        "not a url",
    ] {
        assert!(
            mgr.add_subscription(url).await.is_err(),
            "{url} must be refused"
        );
    }
    assert!(mgr.store().list_subscriptions().unwrap().is_empty());
}

/// Even a `file://` subscription that reached the store some other way (an
/// older bridge, a hand-edited database) is never read by the production
/// fetcher. Uses a small fixture so a regression fails instead of hanging.
#[tokio::test]
async fn the_production_fetcher_never_reads_a_stored_file_url() {
    let fixture = std::env::current_dir()
        .unwrap()
        .join("../../tests/fixtures/blocklists/domains-tiny.txt")
        .canonicalize()
        .unwrap();
    let store = store_with("local", &format!("file://{}", fixture.display()));
    let mgr = BlocklistsManager::new(store.clone());
    assert!(matches!(
        mgr.refresh_now("local").await.unwrap(),
        FetchStatus::Failed { .. }
    ));
    assert!(store.list_entries("local").unwrap().is_empty());
}

#[test]
fn storage_status_defaults_to_not_persistent() {
    assert!(!manager().storage_status().persistent);
    let persistent = StorageStatus {
        unreadable: false,
        persistent: true,
        reason: None,
    };
    let mgr = manager().with_storage_status(persistent.clone());
    assert_eq!(mgr.storage_status(), &persistent);
}

#[tokio::test]
async fn rejecting_a_subscription_emits_an_event_and_stores_nothing() {
    let mgr = manager();
    let mut rx = mgr.subscribe();
    mgr.reject_subscription("http://x.example/hosts", "only https");
    match rx.recv().await.unwrap() {
        BlocklistEvent::SubscriptionRejected { url, reason } => {
            assert_eq!(url, "http://x.example/hosts");
            assert_eq!(reason, "only https");
        }
        other => panic!("expected SubscriptionRejected, got {other:?}"),
    }
    assert!(mgr.store().list_subscriptions().unwrap().is_empty());
}

/// A sink that counts pushes and says whether it installs rules.
struct CountingSink {
    installs: bool,
    pushes: std::sync::atomic::AtomicUsize,
}

impl CountingSink {
    fn installing() -> Self {
        Self {
            installs: true,
            pushes: Default::default(),
        }
    }
}

#[async_trait]
impl super::RuleSink for CountingSink {
    fn unavailable_reason(&self) -> Option<String> {
        (!self.installs).then(|| NO_RULE_SINK_REASON.to_string())
    }

    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        self.pushes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// Nothing is pushed to a sink that installs nothing; its reason is shown.
#[tokio::test]
async fn nothing_is_pushed_while_the_sink_installs_nothing() {
    let sink = Arc::new(CountingSink {
        installs: false,
        pushes: Default::default(),
    });
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    assert_eq!(mgr.refresh_now("tiny").await.unwrap(), FetchStatus::Ok);
    assert_eq!(sink.pushes.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        mgr.enforcement("tiny"),
        Enforcement::NotEnforced {
            reason: NO_RULE_SINK_REASON.to_string()
        }
    );
}

/// M1: after a restart, a downloaded list still says blocking isn't
/// available, in plain language.
#[tokio::test]
async fn a_downloaded_list_reads_not_enforced_after_a_restart() {
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr =
        BlocklistsManager::new(store.clone()).with_fetcher(Arc::new(FixtureFetcher::default()));
    mgr.refresh_now("tiny").await.unwrap();
    drop(mgr);
    let restarted = BlocklistsManager::new(store);
    assert!(restarted.subscription("tiny").unwrap().entry_count > 0);
    assert_eq!(
        restarted.enforcement("tiny"),
        Enforcement::NotEnforced {
            reason: "Blocking isn't available yet".to_string()
        }
    );
}

#[tokio::test]
async fn at_most_max_subscriptions_are_kept() {
    let mgr = manager();
    for i in 0..MAX_SUBSCRIPTIONS {
        assert!(matches!(
            mgr.subscribe_url(&format!("https://x.example/{i}.txt"))
                .await,
            SubscribeOutcome::Added(_)
        ));
    }
    match mgr.subscribe_url("https://x.example/one-more.txt").await {
        SubscribeOutcome::Refused(reason) => assert!(reason.contains("Too many")),
        other => panic!("expected Refused, got {other:?}"),
    }
    assert_eq!(mgr.subscriptions().len(), MAX_SUBSCRIPTIONS);
    assert!(matches!(
        mgr.subscribe_url("https://x.example/0.txt").await,
        SubscribeOutcome::AlreadySubscribed(_)
    ));
}

/// S6: an id already held by a different URL is refused, not overwritten.
#[tokio::test]
async fn an_id_held_by_another_url_is_refused() {
    let url = "https://x.example/hosts";
    let store = store_with(&derive_id(url), "https://other.example/hosts");
    let mgr = BlocklistsManager::new(store.clone());
    assert!(matches!(
        mgr.subscribe_url(url).await,
        SubscribeOutcome::Refused(_)
    ));
    assert_eq!(
        store
            .get_subscription(&derive_id(url))
            .unwrap()
            .unwrap()
            .url,
        "https://other.example/hosts"
    );
}

/// L6: subscribing to the same URL again neither resets nor re-downloads it.
#[tokio::test]
async fn resubscribing_the_same_url_is_a_no_op() {
    use crate::translator::upstream::{handle_blocklist_action, BlocklistActionOutcome};
    use crate::ws_messages::ClientMessage;
    let fetcher = Arc::new(FixtureFetcher::default());
    let mgr = Arc::new(manager().with_fetcher(fetcher.clone()));
    let subscribe = || ClientMessage::SubscribeBlocklist {
        url: fixture_url("domains-tiny.txt"),
    };
    let first = handle_blocklist_action(mgr.clone(), subscribe())
        .await
        .unwrap();
    let BlocklistActionOutcome::Subscribed { id } = first else {
        panic!("expected Subscribed, got {first:?}");
    };
    let before = mgr.subscription(&id).unwrap();
    let again = handle_blocklist_action(mgr.clone(), subscribe())
        .await
        .unwrap();
    assert_eq!(
        again,
        BlocklistActionOutcome::AlreadySubscribed { id: id.clone() }
    );
    assert_eq!(mgr.subscription(&id).unwrap(), before);
    assert_eq!(fetcher.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// M3: summaries come from memory, so the bridge's snapshot path never waits
/// on the store lock a large list write holds.
#[tokio::test]
async fn summaries_never_wait_for_the_store_lock() {
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr = Arc::new(BlocklistsManager::new(store.clone()));
    let _held = store.lock_for_test();
    let (tx, rx) = std::sync::mpsc::channel();
    let builder = mgr.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let built = rt.block_on(crate::translator::downstream::build_set_blocklists(
            &builder,
        ));
        let _ = tx.send(built.is_ok());
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(true),
        "building SetBlocklists waited on the store lock"
    );
}

/// L8: a failed list is retried after a backoff, not on every tick, and a
/// failure is not recorded as a fetch.
#[tokio::test]
async fn a_failed_download_backs_off_before_retrying() {
    let store = store_with("gone", &fixture_url("does-not-exist.txt"));
    let mgr =
        BlocklistsManager::new(store.clone()).with_fetcher(Arc::new(FixtureFetcher::default()));
    assert_eq!(mgr.due_subscription_ids(), vec!["gone".to_string()]);
    mgr.refresh_now("gone").await.unwrap();
    assert!(mgr.due_subscription_ids().is_empty(), "retried at once");
    let stored = store.get_subscription("gone").unwrap().unwrap();
    assert!(stored.last_fetched_at.is_none(), "a failure is not a fetch");
    assert!(stored.last_attempt_at.is_some());
    let mut aged = stored;
    aged.last_attempt_at = Some(Utc::now() - chrono::Duration::seconds(FAILED_RETRY_SECS + 1));
    store.upsert_subscription(&aged).unwrap();
    let restarted = BlocklistsManager::new(store);
    assert_eq!(restarted.due_subscription_ids(), vec!["gone".to_string()]);
}

/// L5: a download that can't be written shows as failed, not "Downloading".
#[tokio::test]
async fn a_store_error_is_shown_on_the_row() {
    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let mgr =
        BlocklistsManager::new(store.clone()).with_fetcher(Arc::new(FixtureFetcher::default()));
    store
        .lock_for_test()
        .execute_batch("DROP TABLE entries;")
        .unwrap();
    let failed = FetchStatus::Failed {
        reason: STORE_ERROR_REASON.to_string(),
    };
    assert_eq!(mgr.refresh_now("tiny").await.unwrap(), failed);
    assert_eq!(mgr.subscription("tiny").unwrap().last_fetch_status, failed);
}

/// Never finishes a fetch: the bridge is "killed" mid-download.
struct HangingFetcher {
    started: tokio::sync::Notify,
}

#[async_trait]
impl fetcher::BlocklistFetch for HangingFetcher {
    async fn fetch(&self, _url: &str) -> fetcher::FetchOutcome {
        self.started.notify_one();
        std::future::pending().await
    }
}

/// The attempt is recorded before the download starts, so a bridge killed
/// mid-download (or mid-parse) backs off on restart instead of refetching
/// the same list at once.
#[tokio::test]
async fn an_interrupted_download_is_not_retried_at_once() {
    let store = store_with("big", &fixture_url("domains-tiny.txt"));
    let fetcher = Arc::new(HangingFetcher {
        started: tokio::sync::Notify::new(),
    });
    let mgr = Arc::new(BlocklistsManager::new(store.clone()).with_fetcher(fetcher.clone()));
    let refreshing = {
        let mgr = mgr.clone();
        tokio::spawn(async move { mgr.refresh_now("big").await })
    };
    fetcher.started.notified().await;
    refreshing.abort();
    let _ = refreshing.await;
    let stored = store.get_subscription("big").unwrap().unwrap();
    assert!(
        stored.last_attempt_at.is_some(),
        "the attempt wasn't saved first"
    );
    let restarted = BlocklistsManager::new(store.clone());
    assert!(
        restarted.due_subscription_ids().is_empty(),
        "an interrupted download is retried at once"
    );
    // After the backoff it is due again.
    let mut aged = stored;
    aged.last_attempt_at = Some(Utc::now() - chrono::Duration::seconds(FAILED_RETRY_SECS + 1));
    store.upsert_subscription(&aged).unwrap();
    assert_eq!(
        BlocklistsManager::new(store).due_subscription_ids(),
        vec!["big".to_string()]
    );
}

/// An interrupted refresh of a list downloaded earlier backs off too.
#[test]
fn an_interrupted_refresh_of_a_downloaded_list_backs_off() {
    let mut sub = Subscription {
        id: "a".into(),
        url: "https://x.example/a".into(),
        display_name: "a".into(),
        format_hint: None,
        refresh_interval_secs: 86_400,
        last_fetched_at: Some(Utc::now() - chrono::Duration::days(2)),
        last_attempt_at: None,
        last_fetch_status: FetchStatus::Ok,
        entry_count: 3,
    };
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    store.upsert_subscription(&sub).unwrap();
    assert_eq!(
        BlocklistsManager::new(store.clone()).due_subscription_ids(),
        vec!["a".to_string()],
        "two days old: due"
    );
    sub.last_attempt_at = Some(Utc::now());
    store.upsert_subscription(&sub).unwrap();
    assert!(
        BlocklistsManager::new(store)
            .due_subscription_ids()
            .is_empty(),
        "attempted just now and never finished: backs off"
    );
}
