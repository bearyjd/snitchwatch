//! [`BlocklistsManager`]: subscriptions, refreshes and their in-memory state.
//!
//! The subscriptions table is mirrored in memory, so summaries for GUIs (and
//! the bridge's snapshot path) never wait on the store lock while a large
//! list is being written. Store work runs on the blocking pool.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::blocklists::fetcher::{
    validate_subscription_url, BlocklistFetch, FetchOutcome, HttpsFetcher, MAX_URL_LEN,
};
use crate::blocklists::leftover::LeftoverRules;
use crate::blocklists::store::{
    BlocklistStore, EntriesPage, FetchStatus, StoreError, Subscription,
};
use crate::blocklists::{
    derive_display_name, derive_id, thousands, BlocklistEvent, Enforcement, NoopRuleSink,
    NotInstalled, RuleSink, AGGREGATE_MAX_HOSTS, FAILED_RETRY_SECS, MAX_SUBSCRIPTIONS,
    NOT_DOWNLOADED_REASON, STORED_MAX_HOSTS, STORE_ERROR_REASON, UNREADABLE_STORE_REASON,
};
use crate::ws_messages::{StorageStatus, BLOCKLIST_ENTRIES_PAGE_MAX};

/// Result of [`BlocklistsManager::subscribe_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscribeOutcome {
    Added(String),
    /// The same URL is already subscribed; nothing changed.
    AlreadySubscribed(String),
    /// Nothing was stored; the reason is shown to the user.
    Refused(String),
}

pub struct BlocklistsManager {
    store: Arc<BlocklistStore>,
    bus: broadcast::Sender<BlocklistEvent>,
    fetcher: Arc<dyn BlocklistFetch>,
    rule_sink: Arc<dyn RuleSink>,
    storage: StorageStatus,
    enforcement: Mutex<HashMap<String, Enforcement>>,
    /// Mirror of the subscriptions table, by id.
    subscriptions: Mutex<BTreeMap<String, Subscription>>,
    /// Ids in the order they were subscribed: the [`AGGREGATE_MAX_HOSTS`]
    /// budget goes to the earliest lists.
    order: Mutex<Vec<String>>,
    /// Why the stored subscriptions couldn't be read at start. While set,
    /// which lists to keep is unknown, so nothing is ever purged.
    load_error: Option<String>,
    /// Where bridge-made blocklist rules nothing manages are found and
    /// removed (issue #73); `None` in tests that don't need it.
    leftover: Option<LeftoverRules>,
    /// The leftover count GUIs were last told, so a change is announced once.
    leftover_announced: Mutex<Option<usize>>,
    /// Lists the daemon refused: how often in a row, and when a refresh tick
    /// may try again (issue #73).
    refusals: Mutex<HashMap<String, backoff::Refusal>>,
    /// The time of day, replaceable in tests.
    clock: Clock,
    /// [`AGGREGATE_MAX_HOSTS`], lowered in tests.
    aggregate_cap: u64,
    /// [`STORED_MAX_HOSTS`], lowered in tests.
    stored_cap: u64,
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

impl BlocklistsManager {
    pub fn new(store: Arc<BlocklistStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        let loaded = store
            .list_subscriptions()
            .and_then(|subs| Ok((subs, store.subscription_order()?)));
        let (subscriptions, order, load_error) = match loaded {
            Ok((subs, order)) => (
                subs.into_iter().map(|s| (s.id.clone(), s)).collect(),
                order,
                None,
            ),
            Err(e) => {
                error!(error = %e, "blocklist store unreadable; no blocklist rule will be removed");
                (BTreeMap::new(), Vec::new(), Some(e.to_string()))
            }
        };
        let manager = Self {
            store,
            bus,
            fetcher: Arc::new(HttpsFetcher::new()),
            rule_sink: Arc::new(NoopRuleSink::default()),
            storage: StorageStatus {
                unreadable: false,
                persistent: false,
                reason: None,
            },
            enforcement: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(subscriptions),
            order: Mutex::new(order),
            load_error,
            leftover: None,
            leftover_announced: Mutex::new(None),
            refusals: Mutex::new(HashMap::new()),
            clock: Arc::new(Utc::now),
            aggregate_cap: AGGREGATE_MAX_HOSTS,
            stored_cap: STORED_MAX_HOSTS,
        };
        let storage = manager.storage.clone();
        manager.with_storage_status(storage)
    }

    /// Replace the default no-op rule sink: a [`daemon_sink::DaemonRuleSink`]
    /// with a state directory, a `NoopRuleSink` saying why without one.
    ///
    /// [`daemon_sink::DaemonRuleSink`]: crate::blocklists::daemon_sink::DaemonRuleSink
    pub fn with_rule_sink(mut self, sink: Arc<dyn RuleSink>) -> Self {
        self.rule_sink = sink;
        self
    }

    /// A lower total size limit. Tests only.
    #[cfg(test)]
    pub(crate) fn with_aggregate_cap(mut self, cap: u64) -> Self {
        self.aggregate_cap = cap;
        self
    }

    /// A lower limit on the hosts saved in all lists. Tests only.
    #[cfg(test)]
    pub(crate) fn with_stored_cap(mut self, cap: u64) -> Self {
        self.stored_cap = cap;
        self
    }

    /// Replace the clock refusal backoff reads. Tests only.
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Where leftover rules are read and removed (issue #73).
    pub fn with_leftover_rules(mut self, leftover: LeftoverRules) -> Self {
        self.leftover = Some(leftover);
        self
    }

    /// Replace the default [`HttpsFetcher`]. Tests only: production never
    /// sets a fetcher, so it has no non-https fetch path.
    pub fn with_fetcher(mut self, fetcher: Arc<dyn BlocklistFetch>) -> Self {
        self.fetcher = fetcher;
        self
    }

    /// Record whether [`store`](Self::store) outlives the bridge process. Sent
    /// to GUIs with every `SetBlocklists`. A store that couldn't be read is
    /// reported as such, whatever `storage` says.
    pub fn with_storage_status(mut self, storage: StorageStatus) -> Self {
        self.storage = match &self.load_error {
            Some(e) => StorageStatus {
                unreadable: true,
                persistent: storage.persistent,
                reason: Some(format!("Couldn't read the saved blocklists: {e}")),
            },
            None => storage,
        };
        self
    }

    pub fn storage_status(&self) -> &StorageStatus {
        &self.storage
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BlocklistEvent> {
        self.bus.subscribe()
    }

    pub fn store(&self) -> &Arc<BlocklistStore> {
        &self.store
    }

    /// Every subscription, by id, from memory (never waits on the store).
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.cache().values().cloned().collect()
    }

    pub fn subscription(&self, id: &str) -> Option<Subscription> {
        self.cache().get(id).cloned()
    }

    pub fn has_subscription(&self, id: &str) -> bool {
        self.cache().contains_key(id)
    }

    fn order(&self) -> MutexGuard<'_, Vec<String>> {
        self.order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The subscription's enforcement state. Without a sink that installs
    /// rules, every list reports the sink's reason, including after a
    /// restart.
    pub fn enforcement(&self, id: &str) -> Enforcement {
        self.enforcement_map()
            .get(id)
            .cloned()
            .unwrap_or_else(|| self.default_enforcement())
    }

    fn default_enforcement(&self) -> Enforcement {
        match self.unavailable_reason() {
            Some(reason) => Enforcement::NotEnforced { reason },
            None => Enforcement::Pending,
        }
    }

    /// Why nothing is installed: an unreadable store (the size limit can't
    /// count the rules already there), or a sink that installs nothing.
    fn unavailable_reason(&self) -> Option<String> {
        if self.load_error.is_some() {
            return Some(UNREADABLE_STORE_REASON.to_string());
        }
        self.rule_sink.unavailable_reason()
    }

    fn installs_rules(&self) -> bool {
        self.unavailable_reason().is_none()
    }

    /// How many blocklist rules Snitchwatch made are in the firewall with
    /// nothing managing them (issue #73): `Some` only when this bridge
    /// installs no rules (no state directory, a per-user service, an
    /// unreadable store), the daemon's rule list is known, and there are
    /// some. With a working sink, rules of lists no longer subscribed are
    /// deleted on their own.
    pub fn leftover_count(&self) -> Option<usize> {
        if self.installs_rules() {
            return None;
        }
        let names = self.leftover.as_ref()?.names()?;
        (!names.is_empty()).then_some(names.len())
    }

    /// Delete the leftover rules, because the user asked. Does nothing while
    /// this bridge manages its rules. Tells GUIs what is left.
    pub async fn remove_leftover_rules(&self) {
        if self.installs_rules() {
            warn!("asked to remove leftover blocklist rules while managing them; ignored");
            return;
        }
        let Some(leftover) = &self.leftover else {
            return;
        };
        match leftover.remove_all().await {
            Ok(done) => info!(
                removed = done.removed,
                refused = done.refused,
                "removed leftover blocklist rules"
            ),
            Err(e) => warn!(reason = %e.reason, "couldn't remove leftover blocklist rules"),
        }
        *self.leftover_announced() = self.leftover_count();
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
    }

    fn leftover_announced(&self) -> MutexGuard<'_, Option<usize>> {
        self.leftover_announced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Send the subscriptions again if the leftover count changed since GUIs
    /// were last told.
    pub(super) fn announce_leftover_change(&self) {
        let now = self.leftover_count();
        let mut told = self.leftover_announced();
        if *told != now {
            *told = now;
            drop(told);
            let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        }
    }

    /// Record a sink outcome (the caller tells GUIs).
    fn record_install(&self, id: &str, outcome: &Result<(), NotInstalled>) {
        let enforcement = match outcome {
            Ok(()) => Enforcement::RuleInstalled { at: Utc::now() },
            Err(e) if e.daemon_unavailable => {
                warn!(%id, reason = %e.reason, "blocklist not confirmed");
                Enforcement::Unconfirmed {
                    reason: e.reason.clone(),
                }
            }
            Err(e) => {
                warn!(%id, reason = %e.reason, "blocklist not enforced");
                Enforcement::NotEnforced {
                    reason: e.reason.clone(),
                }
            }
        };
        self.enforcement_map().insert(id.to_string(), enforcement);
    }

    fn enforcement_map(&self) -> MutexGuard<'_, HashMap<String, Enforcement>> {
        self.enforcement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cache(&self) -> MutexGuard<'_, BTreeMap<String, Subscription>> {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Run `work` against the store on the blocking pool.
    async fn with_store<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&BlocklistStore) -> Result<T, StoreError> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?
    }

    /// Store a new subscription (does not fetch). Refuses, storing nothing, a
    /// URL [`validate_subscription_url`] rejects, a 33rd list, and an id
    /// already used by a different URL. The same URL again is a no-op.
    pub async fn subscribe_url(&self, url: &str) -> SubscribeOutcome {
        let url = match validate_subscription_url(url) {
            Ok(parsed) => parsed.to_string(),
            Err(reason) => return SubscribeOutcome::Refused(reason),
        };
        let id = derive_id(&url);
        {
            let cache = self.cache();
            if let Some(existing) = cache.get(&id) {
                return if existing.url == url {
                    SubscribeOutcome::AlreadySubscribed(id)
                } else {
                    SubscribeOutcome::Refused(
                        "Couldn't add this list: another list has the same ID".to_string(),
                    )
                };
            }
            if cache.len() >= MAX_SUBSCRIPTIONS {
                return SubscribeOutcome::Refused(format!(
                    "Too many blocklists (at most {MAX_SUBSCRIPTIONS})"
                ));
            }
        }
        let sub = Subscription {
            id: id.clone(),
            display_name: derive_display_name(&url),
            url,
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_attempt_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        let row = sub.clone();
        if let Err(e) = self.with_store(move |s| s.upsert_subscription(&row)).await {
            error!(%id, error = %e, "couldn't store blocklist subscription");
            return SubscribeOutcome::Refused("Couldn't save the subscription".to_string());
        }
        self.cache().insert(id.clone(), sub);
        self.order().push(id.clone());
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        SubscribeOutcome::Added(id)
    }

    /// [`subscribe_url`](Self::subscribe_url) as a `Result`: the id, or the
    /// refusal reason as an error.
    pub async fn add_subscription(&self, url: &str) -> anyhow::Result<String> {
        match self.subscribe_url(url).await {
            SubscribeOutcome::Added(id) | SubscribeOutcome::AlreadySubscribed(id) => Ok(id),
            SubscribeOutcome::Refused(reason) => anyhow::bail!("refused blocklist URL: {reason}"),
        }
    }

    /// Tell GUIs a subscribe request was refused. Nothing is stored; the URL
    /// echoed back is capped.
    pub fn reject_subscription(&self, url: &str, reason: &str) {
        let _ = self.bus.send(BlocklistEvent::SubscriptionRejected {
            url: url.chars().take(MAX_URL_LEN).collect(),
            reason: reason.to_string(),
        });
    }

    /// Resend the subscription list (e.g. to clear a refused row).
    pub fn announce_subscriptions(&self) {
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
    }

    /// Forget `id`, then delete its daemon rules and only then its files
    /// (issue #45). A rule the daemon couldn't delete now is an orphan the
    /// next [`reconcile`](Self::reconcile) deletes.
    pub async fn remove_subscription(&self, id: &str) -> anyhow::Result<()> {
        let owned = id.to_string();
        self.with_store(move |s| s.delete_subscription(&owned))
            .await?;
        self.cache().remove(id);
        self.order().retain(|other| other != id);
        self.enforcement_map().remove(id);
        self.forget_refusals(id);
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        if self.installs_rules() {
            if let Err(e) = self.rule_sink.release_blocklist_rules(id).await {
                warn!(%id, reason = %e.reason, "couldn't remove an unsubscribed list's rules");
            }
        }
        Ok(())
    }

    /// Ask the event pump for a page of `id`'s hosts (at most
    /// [`BLOCKLIST_ENTRIES_PAGE_MAX`]), to be sent with `request_id`.
    pub fn request_entries(&self, id: &str, offset: u64, limit: u32, request_id: Option<String>) {
        let _ = self.bus.send(BlocklistEvent::EntriesRequested {
            subscription_id: id.to_string(),
            offset,
            limit,
            request_id,
        });
    }

    /// A page of `id`'s hosts, its total entry count and the download it came
    /// from, read together. Never more than [`BLOCKLIST_ENTRIES_PAGE_MAX`]
    /// hosts.
    pub async fn entries_page(
        &self,
        id: &str,
        offset: u64,
        limit: u32,
    ) -> anyhow::Result<EntriesPage> {
        let limit = limit.clamp(1, BLOCKLIST_ENTRIES_PAGE_MAX);
        let owned = id.to_string();
        self.with_store(move |s| s.entries_page(&owned, offset, limit))
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown subscription: {id}"))
    }

    /// Download `id` now and update the store, memory and GUIs. A failed
    /// download keeps the cached entries.
    pub async fn refresh_now(&self, id: &str) -> anyhow::Result<FetchStatus> {
        let Some(mut sub) = self.subscription(id) else {
            anyhow::bail!("unknown subscription: {id}");
        };
        // Saved before the download: a bridge killed mid-download or
        // mid-parse backs off on restart instead of retrying at once.
        sub.last_attempt_at = Some(Utc::now());
        let row = sub.clone();
        match self.with_store(move |s| s.update_subscription(&row)).await {
            Ok(false) => anyhow::bail!("subscription removed: {id}"),
            Ok(true) => {}
            Err(e) => error!(%id, error = %e, "couldn't record a download attempt"),
        }
        self.cache().insert(sub.id.clone(), sub.clone());
        let outcome = match self.fetcher.fetch(&sub.url).await {
            FetchOutcome::Ok { hosts, .. } if self.over_stored_limit(&sub.id, hosts.len()) => {
                FetchOutcome::Failed {
                    reason: format!(
                        "Saving this list would pass the limit of {} hosts across all lists",
                        thousands(self.stored_cap)
                    ),
                }
            }
            outcome => outcome,
        };
        let now = Utc::now();
        match outcome {
            FetchOutcome::Ok { hosts, .. } => Ok(self.store_download(sub, hosts, now).await),
            FetchOutcome::Failed { reason } => {
                let mut updated = sub;
                updated.last_attempt_at = Some(now);
                updated.last_fetch_status = FetchStatus::Failed {
                    reason: reason.clone(),
                };
                let row = updated.clone();
                match self.with_store(move |s| s.update_subscription(&row)).await {
                    Ok(false) => return Ok(updated.last_fetch_status),
                    Ok(true) => {}
                    Err(e) => {
                        error!(id = %updated.id, error = %e, "couldn't record a failed download")
                    }
                }
                if self.installs_rules() {
                    self.enforcement_map()
                        .entry(updated.id.clone())
                        .or_insert_with(|| Enforcement::NotEnforced {
                            reason: NOT_DOWNLOADED_REASON.to_string(),
                        });
                }
                self.cache().insert(updated.id.clone(), updated.clone());
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: updated.id.clone(),
                });
                warn!(id = %updated.id, %reason, "blocklist refresh failed; cache preserved");
                Ok(FetchStatus::Failed { reason })
            }
        }
    }

    /// Whether saving `hosts` hosts for `id` would take the hosts saved in
    /// all lists past [`STORED_MAX_HOSTS`]. `id`'s own old copy is replaced,
    /// so only the other lists count.
    fn over_stored_limit(&self, id: &str, hosts: usize) -> bool {
        let others: u64 = self
            .cache()
            .values()
            .filter(|s| s.id != id)
            .map(|s| u64::try_from(s.entry_count).unwrap_or(0))
            .sum();
        others.saturating_add(hosts as u64) > self.stored_cap
    }

    async fn store_download(
        &self,
        sub: Subscription,
        hosts: Vec<String>,
        now: DateTime<Utc>,
    ) -> FetchStatus {
        let mut updated = sub;
        updated.entry_count = hosts.len() as i64;
        updated.last_fetched_at = Some(now);
        updated.last_attempt_at = Some(now);
        updated.last_fetch_status = FetchStatus::Ok;
        let row = updated.clone();
        let count = hosts.len();
        // The hosts come back out of the blocking task for the sink, so up
        // to `MAX_ENTRIES` strings are never cloned.
        let hosts = match self
            .with_store(move |s| {
                s.replace_entries_and_update(&row, &hosts)
                    .map(|ok| (ok, hosts))
            })
            .await
        {
            Ok((true, hosts)) => hosts,
            Ok((false, _)) => return FetchStatus::Ok,
            Err(e) => {
                error!(id = %updated.id, error = %e, "couldn't store a downloaded blocklist");
                updated.entry_count = self.subscription(&updated.id).map_or(0, |s| s.entry_count);
                updated.last_fetched_at = None;
                updated.last_fetch_status = FetchStatus::Failed {
                    reason: STORE_ERROR_REASON.to_string(),
                };
                let status = updated.last_fetch_status.clone();
                let id = updated.id.clone();
                self.cache().insert(id.clone(), updated);
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: id,
                });
                return status;
            }
        };
        let id = updated.id.clone();
        self.cache().insert(id.clone(), updated);
        if self.installs_rules() {
            // This list may have grown past what later lists left room for:
            // take those off the daemon before this one's files change, so it
            // never holds more than the limit, even for a moment.
            self.demote_lists_past_the_limit(&id).await;
            let outcome = self.install(&id, hosts).await;
            self.record_install(&id, &outcome);
        }
        let _ = self.bus.send(BlocklistEvent::EntriesChanged {
            subscription_id: id.clone(),
        });
        let _ = self.bus.send(BlocklistEvent::StatusChanged {
            subscription_id: id.clone(),
        });
        info!(%id, count, "blocklist refreshed");
        FetchStatus::Ok
    }

    /// Ids whose refresh interval elapsed, that were never downloaded, or
    /// whose failed download is past its retry backoff.
    pub fn due_subscription_ids(&self) -> Vec<String> {
        let now = Utc::now();
        self.cache()
            .values()
            .filter(|s| is_due(s, now))
            .map(|s| s.id.clone())
            .collect()
    }
}

/// Never attempted: due. Last attempt succeeded: due after the refresh
/// interval. Last attempt failed or never finished (the bridge stopped
/// mid-download): due after the retry backoff.
fn is_due(sub: &Subscription, now: DateTime<Utc>) -> bool {
    let Some(attempt) = sub.last_attempt_at.or(sub.last_fetched_at) else {
        return true;
    };
    let succeeded = sub
        .last_fetched_at
        .filter(|fetched| *fetched >= attempt && sub.last_fetch_status == FetchStatus::Ok);
    match succeeded {
        Some(fetched) => (now - fetched).num_seconds() >= sub.refresh_interval_secs,
        None => (now - attempt).num_seconds() >= FAILED_RETRY_SECS.min(sub.refresh_interval_secs),
    }
}

#[path = "manager_backoff.rs"]
mod backoff;
#[path = "manager_reconcile.rs"]
mod reconcile;
