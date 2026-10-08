//! Unit tests for `BlocklistsManager` (split out of `mod.rs` for size).
use super::test_helpers::{fixture_url, FixtureFetcher};
use super::*;

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
    assert!(mgr.store.get_subscription(&id).unwrap().is_none());
}

/// Ids are persisted and (PR B) become rule names and list directories, so
/// the exact format is pinned: `<sanitized stem>-<8 hex of SHA-256(url)>`.
#[test]
fn derive_id_is_the_stem_plus_a_url_hash() {
    assert_eq!(
        derive_id("https://x.example/StevenBlack/hosts"),
        "hosts-0a83af59"
    );
    assert_eq!(
        derive_id("https://x.example/hosts.txt?branch=main"),
        "hosts-de29c990"
    );
    assert_eq!(derive_id("https://x.example/"), "list-37714a19");
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
            id.len() <= 64 + 9,
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
async fn refresh_pushes_materialized_rules_to_sink() {
    use crate::blocklists::materializer::MaterializedRule;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct CapturingSink {
        calls: StdMutex<Vec<Vec<MaterializedRule>>>,
    }

    #[async_trait]
    impl super::RuleSink for CapturingSink {
        async fn replace_blocklist_rules(
            &self,
            _list_id: &str,
            rules: Vec<MaterializedRule>,
        ) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(rules);
            Ok(())
        }
    }

    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));
    let sink: Arc<CapturingSink> = Arc::new(CapturingSink::default());
    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    mgr.refresh_now("tiny").await.unwrap();
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "expected one push call");
    assert!(!calls[0].is_empty(), "domains-tiny.txt should have hosts");
    assert!(calls[0][0].name.starts_with("z00-blocklist:tiny:"));
    assert!(
        matches!(mgr.enforcement("tiny"), Enforcement::RuleInstalled { .. }),
        "a sink that accepted the rules reports them installed"
    );
}

#[tokio::test]
async fn refresh_removes_legacy_band_rules() {
    // A sink that models a daemon's name-keyed rule set and honors the
    // `RuleSink` replace contract: on each replace it purges every existing
    // rule under the list's owned prefixes (current + legacy band), then
    // installs the fresh set. Proves an upgrade off the old "900-blocklist:"
    // band leaves no orphaned old-prefix denies.
    use crate::blocklists::materializer::{owned_blocklist_rule_name_prefixes, MaterializedRule};
    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct RuleStoreSink {
        rules: StdMutex<BTreeMap<String, MaterializedRule>>,
    }

    #[async_trait]
    impl super::RuleSink for RuleStoreSink {
        async fn replace_blocklist_rules(
            &self,
            list_id: &str,
            rules: Vec<MaterializedRule>,
        ) -> anyhow::Result<()> {
            let mut map = self.rules.lock().unwrap();
            let owned = owned_blocklist_rule_name_prefixes(list_id);
            map.retain(|name, _| !owned.iter().any(|p| name.starts_with(p.as_str())));
            for rule in rules {
                map.insert(rule.name.clone(), rule);
            }
            Ok(())
        }
    }

    let store = store_with("tiny", &fixture_url("domains-tiny.txt"));

    let sink: Arc<RuleStoreSink> = Arc::new(RuleStoreSink::default());
    // Seed the daemon with an orphaned legacy-band rule, as a pre-upgrade
    // daemon would hold, plus an unrelated user rule that must survive.
    {
        let mut map = sink.rules.lock().unwrap();
        map.insert(
            "900-blocklist:tiny:0000-doubleclick.net".to_string(),
            MaterializedRule {
                name: "900-blocklist:tiny:0000-doubleclick.net".to_string(),
                enabled: true,
                action: "deny".to_string(),
                duration: "always".to_string(),
                description: String::new(),
                operator: super::materializer::Operator {
                    kind: "simple".to_string(),
                    operand: "dest.host".to_string(),
                    data: "doubleclick.net".to_string(),
                },
            },
        );
        map.insert(
            "899-firefox-allow-out".to_string(),
            MaterializedRule {
                name: "899-firefox-allow-out".to_string(),
                enabled: true,
                action: "allow".to_string(),
                duration: "always".to_string(),
                description: String::new(),
                operator: super::materializer::Operator {
                    kind: "simple".to_string(),
                    operand: "process.path".to_string(),
                    data: "/usr/bin/firefox".to_string(),
                },
            },
        );
    }

    let mgr = BlocklistsManager::new(store)
        .with_fetcher(Arc::new(FixtureFetcher::default()))
        .with_rule_sink(sink.clone());
    mgr.refresh_now("tiny").await.unwrap();

    let map = sink.rules.lock().unwrap();
    assert!(
        !map.keys().any(|n| n.starts_with("900-blocklist:tiny:")),
        "legacy-band rules must be purged on refresh: {:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(
        map.keys().any(|n| n.starts_with("z00-blocklist:tiny:")),
        "current-band rules must be installed: {:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(
        map.contains_key("899-firefox-allow-out"),
        "unrelated user rules must survive the replace"
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
            last_fetch_status: FetchStatus::Ok,
            entry_count: count_before as i64,
        })
        .unwrap();
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
    assert_eq!(mgr.enforcement("tiny"), Enforcement::Pending);
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
            _rules: Vec<materializer::MaterializedRule>,
        ) -> anyhow::Result<()> {
            anyhow::bail!("daemon said no")
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
    let mgr = BlocklistsManager::new(store).with_fetcher(Arc::new(FixtureFetcher::default()));
    mgr.refresh_now("tiny").await.unwrap();
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
