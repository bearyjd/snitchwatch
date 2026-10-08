//! Issue #73: a list the daemon refuses is retried on a growing schedule, not
//! on every refresh tick, and the reason says so.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};

use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;

/// A sink that refuses (or, once told, accepts) and counts what it is asked.
#[derive(Default)]
struct Sink {
    accept: AtomicBool,
    unavailable: AtomicBool,
    pushes: AtomicUsize,
    reinstalls: AtomicUsize,
    verified: AtomicBool,
}

impl Sink {
    fn outcome(&self) -> Result<(), NotInstalled> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(NotInstalled::daemon_unavailable("didn't answer"))
        } else if self.accept.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(NotInstalled::new("The firewall service refused the rule"))
        }
    }
}

#[async_trait]
impl RuleSink for Sink {
    fn files_verified(&self, _list_id: &str) -> bool {
        self.verified.load(Ordering::SeqCst)
    }

    async fn reinstall_blocklist_rules(&self, _list_id: &str) -> Result<(), NotInstalled> {
        self.reinstalls.fetch_add(1, Ordering::SeqCst);
        self.outcome()
    }

    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        self.pushes.fetch_add(1, Ordering::SeqCst);
        self.outcome()
    }
}

struct Fixture {
    sink: Arc<Sink>,
    mgr: BlocklistsManager,
    now: Arc<StdMutex<DateTime<Utc>>>,
}

impl Fixture {
    fn new() -> Self {
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        store
            .upsert_subscription(&Subscription {
                id: "ads".into(),
                url: "https://example.invalid/ads".into(),
                display_name: "ads".into(),
                format_hint: None,
                refresh_interval_secs: 86_400,
                last_fetched_at: Some(Utc::now()),
                last_attempt_at: None,
                last_fetch_status: FetchStatus::Ok,
                entry_count: 2,
            })
            .unwrap();
        store
            .replace_entries("ads", &["a.example", "b.example"])
            .unwrap();
        let sink = Arc::new(Sink::default());
        let now = Arc::new(StdMutex::new(
            Utc.with_ymd_and_hms(2026, 10, 9, 8, 0, 0).unwrap(),
        ));
        let clock = now.clone();
        let mgr = BlocklistsManager::new(store)
            .with_rule_sink(sink.clone())
            .with_clock(Arc::new(move || *clock.lock().unwrap()));
        Self { sink, mgr, now }
    }

    fn advance(&self, minutes: i64) {
        *self.now.lock().unwrap() += Duration::minutes(minutes);
    }

    fn pushes(&self) -> usize {
        self.sink.pushes.load(Ordering::SeqCst) + self.sink.reinstalls.load(Ordering::SeqCst)
    }

    fn reason(&self) -> String {
        match self.mgr.enforcement("ads") {
            Enforcement::NotEnforced { reason } => reason,
            other => panic!("expected NotEnforced, got {other:?}"),
        }
    }

    /// A refresh tick's reconcile.
    async fn tick(&self) {
        self.mgr.reconcile_after_refresh(&[]).await;
    }
}

#[tokio::test]
async fn a_refused_list_is_retried_after_15_minutes_then_an_hour_then_four() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert_eq!(f.pushes(), 1);
    assert!(f
        .reason()
        .starts_with("The firewall service refused the rule"));
    assert!(f.reason().contains("about 15 minutes"), "{}", f.reason());

    f.advance(14);
    f.tick().await;
    assert_eq!(f.pushes(), 1, "14 minutes in: still backing off");
    f.advance(2);
    f.tick().await;
    assert_eq!(f.pushes(), 2);
    assert!(f.reason().contains("about 1 hour"), "{}", f.reason());

    f.advance(59);
    f.tick().await;
    assert_eq!(f.pushes(), 2);
    f.advance(2);
    f.tick().await;
    assert_eq!(f.pushes(), 3);
    assert!(f.reason().contains("about 4 hours"), "{}", f.reason());

    f.advance(239);
    f.tick().await;
    assert_eq!(f.pushes(), 3);
    f.advance(2);
    f.tick().await;
    assert_eq!(f.pushes(), 4);
    assert!(
        f.reason().contains("about 4 hours"),
        "the schedule stops growing: {}",
        f.reason()
    );
}

#[tokio::test]
async fn the_reason_keeps_the_daemons_own_words() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert_eq!(
        f.reason(),
        "The firewall service refused the rule. Snitchwatch will try again in about 15 minutes."
    );
}

#[tokio::test]
async fn a_new_daemon_rule_list_retries_at_once() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    f.mgr.reconcile().await;
    assert_eq!(
        f.pushes(),
        2,
        "a Full pass outside a tick ignores the backoff"
    );
    assert!(
        f.reason().contains("about 1 hour"),
        "and counts as another refusal"
    );
}

#[tokio::test]
async fn success_ends_the_backoff() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    f.sink.accept.store(true, Ordering::SeqCst);
    f.mgr.reconcile().await;
    assert!(matches!(
        f.mgr.enforcement("ads"),
        Enforcement::RuleInstalled { .. }
    ));
    // Refused again later: the schedule starts over at 15 minutes.
    f.sink.accept.store(false, Ordering::SeqCst);
    f.mgr.reconcile().await;
    assert!(f.reason().contains("about 15 minutes"), "{}", f.reason());
}

#[tokio::test]
async fn an_unreachable_daemon_is_not_a_refusal_and_is_retried_every_tick() {
    let f = Fixture::new();
    f.sink.unavailable.store(true, Ordering::SeqCst);
    f.mgr.reconcile().await;
    f.tick().await;
    f.tick().await;
    assert_eq!(f.pushes(), 3, "no backoff while the daemon doesn't answer");
    assert!(matches!(
        f.mgr.enforcement("ads"),
        Enforcement::Unconfirmed { .. }
    ));
    // And the first real refusal after it is the first.
    f.sink.unavailable.store(false, Ordering::SeqCst);
    f.mgr.reconcile().await;
    assert!(f.reason().contains("about 15 minutes"), "{}", f.reason());
}

#[tokio::test]
async fn a_refused_list_is_not_read_from_the_store_while_it_backs_off() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    f.sink.verified.store(true, Ordering::SeqCst);
    f.advance(1);
    // Were it tried, it would be resent over its verified files.
    f.tick().await;
    assert_eq!(f.sink.reinstalls.load(Ordering::SeqCst), 0);
    f.advance(15);
    f.tick().await;
    assert_eq!(f.sink.reinstalls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsubscribing_forgets_the_schedule() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert!(f.mgr.refusal_state("ads").is_some());
    f.mgr.remove_subscription("ads").await.unwrap();
    assert!(f.mgr.refusal_state("ads").is_none());
}
