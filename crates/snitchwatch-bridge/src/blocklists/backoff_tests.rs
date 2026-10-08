//! Issue #73: a list the daemon refuses is retried on a growing schedule, not
//! on every refresh tick, and the reason says so.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;

use super::store::{BlocklistStore, FetchStatus, Subscription};
use super::*;

/// A sink that refuses (or, once told, accepts) and counts what it is asked.
#[derive(Default)]
struct Sink {
    accept: AtomicBool,
    unavailable: AtomicBool,
    no_hosts: AtomicBool,
    cleanup_pending: AtomicBool,
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
        } else if self.cleanup_pending.load(Ordering::SeqCst) {
            Err(NotInstalled::cleanup_pending("an old rule is still there"))
        } else if self.no_hosts.load(Ordering::SeqCst) {
            Err(NotInstalled::new(NO_HOSTS_REASON))
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
        let mgr = BlocklistsManager::new(store).with_rule_sink(sink.clone());
        Self { sink, mgr }
    }

    /// Time passes (the tests run on a paused clock).
    async fn advance(&self, minutes: u64) {
        tokio::time::advance(Duration::from_secs(minutes * 60)).await;
    }

    async fn advance_secs(&self, secs: u64) {
        tokio::time::advance(Duration::from_secs(secs)).await;
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

/// A tick finds the list still backing off a second before the minute of
/// slack runs out, and due at it. (Slack: a refusal is stamped a little after
/// its tick began, so the tick one period later is a little early.)
async fn assert_retried_after(f: &Fixture, secs: u64) {
    let before = f.pushes();
    f.advance_secs(secs - 1).await;
    f.tick().await;
    assert_eq!(f.pushes(), before, "{} s in: still backing off", secs - 1);
    f.advance_secs(1).await;
    f.tick().await;
    assert_eq!(f.pushes(), before + 1, "{secs} s in: due");
}

#[tokio::test(start_paused = true)]
async fn a_refused_list_is_retried_after_15_minutes_then_an_hour_then_four() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert_eq!(f.pushes(), 1);
    assert!(f
        .reason()
        .starts_with("The firewall service refused the rule"));
    assert!(f.reason().ends_with("about 15 minutes."), "{}", f.reason());

    // Due a minute early, the slack, and not before.
    assert_retried_after(&f, 14 * 60).await;
    assert!(f.reason().ends_with("about 1 hour."), "{}", f.reason());
    assert_retried_after(&f, 59 * 60).await;
    assert!(f.reason().ends_with("about 4 hours."), "{}", f.reason());
    assert_retried_after(&f, 239 * 60).await;
    assert!(
        f.reason().ends_with("about 4 hours."),
        "the schedule stops growing: {}",
        f.reason()
    );
    assert_eq!(f.pushes(), 4);
}

/// The reported bug: the install that refused a list finished a few seconds
/// after its tick began, so the tick exactly 15 minutes later skipped it and
/// the schedule ran a whole tick long.
#[tokio::test(start_paused = true)]
async fn the_tick_one_period_after_a_refusal_retries_the_list() {
    let f = Fixture::new();
    // The tick began at T; the install finished, and was refused, at T + 20 s.
    f.advance_secs(20).await;
    f.mgr.reconcile().await;
    assert_eq!(f.pushes(), 1);
    // The next tick is at exactly T + 15 minutes.
    f.advance_secs(15 * 60 - 20).await;
    f.tick().await;
    assert_eq!(f.pushes(), 2, "retried on the tick after 15 minutes");
}

#[tokio::test(start_paused = true)]
async fn the_reason_keeps_the_daemons_own_words() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert_eq!(
        f.reason(),
        "The firewall service refused the rule. Snitchwatch will try again in about 15 minutes."
    );
}

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
async fn a_refused_list_is_not_read_from_the_store_while_it_backs_off() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    f.sink.verified.store(true, Ordering::SeqCst);
    f.advance(1).await;
    // Were it tried, it would be resent over its verified files.
    f.tick().await;
    assert_eq!(f.sink.reinstalls.load(Ordering::SeqCst), 0);
    f.advance(15).await;
    f.tick().await;
    assert_eq!(f.sink.reinstalls.load(Ordering::SeqCst), 1);
}

/// Retrying a list with nothing a rule can match changes nothing until its
/// next download, so it gets no schedule and no "try again" promise.
#[tokio::test(start_paused = true)]
async fn a_list_with_no_blockable_hosts_is_not_scheduled_for_a_retry() {
    let f = Fixture::new();
    f.sink.no_hosts.store(true, Ordering::SeqCst);
    f.mgr.reconcile().await;
    assert_eq!(f.reason(), NO_HOSTS_REASON);
    assert!(f.mgr.refusal_state("ads").is_none());
}

/// A list whose last try went unanswered isn't "refused" any more, so the
/// next tick tries it even inside the old schedule.
#[tokio::test(start_paused = true)]
async fn a_list_the_daemon_stopped_answering_for_is_tried_by_the_next_tick() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert!(f.mgr.refusal_state("ads").is_some());
    f.sink.unavailable.store(true, Ordering::SeqCst);
    f.mgr.reconcile().await;
    assert!(matches!(
        f.mgr.enforcement("ads"),
        Enforcement::Unconfirmed { .. }
    ));
    let before = f.pushes();
    f.advance(1).await; // well inside the 15 minutes
    f.tick().await;
    assert_eq!(f.pushes(), before + 1);
}

#[tokio::test(start_paused = true)]
async fn unsubscribing_forgets_the_schedule() {
    let f = Fixture::new();
    f.mgr.reconcile().await;
    assert!(f.mgr.refusal_state("ads").is_some());
    f.mgr.remove_subscription("ads").await.unwrap();
    assert!(f.mgr.refusal_state("ads").is_none());
}

/// Trouble cleaning up an old kind is not a refusal of the list: it reads
/// "not confirmed", has no schedule, and every tick tries it again.
#[tokio::test(start_paused = true)]
async fn a_pending_cleanup_is_not_a_refusal_and_is_retried_every_tick() {
    let f = Fixture::new();
    f.sink.cleanup_pending.store(true, Ordering::SeqCst);
    f.mgr.reconcile().await;
    assert!(matches!(
        f.mgr.enforcement("ads"),
        Enforcement::Unconfirmed { .. }
    ));
    assert!(f.mgr.refusal_state("ads").is_none());
    f.tick().await;
    f.tick().await;
    assert_eq!(f.pushes(), 3);
}

struct Serves;

#[async_trait]
impl super::fetcher::BlocklistFetch for Serves {
    async fn fetch(&self, _url: &str) -> super::fetcher::FetchOutcome {
        super::fetcher::process_body("0.0.0.0 x.example\n0.0.0.0 y.example\n")
    }
}

/// A list taken off the daemon for the size limit starts a schedule of its
/// own when it is next refused, not one left over from before.
#[tokio::test(start_paused = true)]
async fn a_list_taken_off_for_the_size_limit_forgets_its_refusals() {
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    for (id, hosts) in [
        ("other", ["a.example"].as_slice()),
        ("ads", ["a.example", "b.example"].as_slice()),
    ] {
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
                entry_count: hosts.len() as i64,
            })
            .unwrap();
        store.replace_entries(id, hosts).unwrap();
    }
    let sink = Arc::new(Sink::default());
    let mgr = BlocklistsManager::new(store)
        .with_rule_sink(sink.clone())
        .with_fetcher(Arc::new(Serves))
        .with_aggregate_cap(3);
    mgr.reconcile().await;
    assert!(mgr.refusal_state("ads").is_some(), "ads was refused");

    // `other` grows to 2 hosts: 2 + 2 > 3, so `ads` comes off the daemon.
    mgr.refresh_now("other").await.unwrap();
    assert!(mgr.refusal_state("ads").is_none());
}
