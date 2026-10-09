//! [`BlocklistsManager`]: subscriptions, refreshes and their in-memory state.
//!
//! The subscriptions table is mirrored in memory, so summaries for GUIs (and
//! the bridge's snapshot path) never wait on the store lock while a large
//! list is being written. Store work runs on the blocking pool.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::blocklists::fetcher::{
    validate_subscription_url, BlocklistFetch, HttpsFetcher, MAX_URL_LEN,
};
use crate::blocklists::leftover::{LeftoverRules, RemovedLeftovers};
use crate::blocklists::store::{
    BlocklistStore, EntriesPage, FetchStatus, StoreError, Subscription,
};
use crate::blocklists::{
    derive_display_name, derive_id, BlocklistEvent, Enforcement, NoopRuleSink, NotInstalled,
    RuleSink, AGGREGATE_MAX_HOSTS, MAX_SUBSCRIPTIONS, STORED_MAX_HOSTS, UNREADABLE_STORE_REASON,
};
use crate::ws_messages::{
    ReplyTo, StorageStatus, BLOCKLIST_ENTRIES_PAGE_MAX, LEFTOVER_CAUSE_NO_STATE_DIR,
    LEFTOVER_CAUSE_STORE_UNREADABLE,
};

/// What to tell the page about a removal that did not remove everything, or
/// `None` if it did. A refused delete counts as removed: the daemon stopped
/// using the rule before it failed to remove the file (tower r12).
fn removal_note(done: &RemovedLeftovers) -> Option<String> {
    let taken = done.removed + done.refused;
    let of = format!("Removed {taken} of {} rules", done.total);
    let files = match done.refused {
        0 => String::new(),
        1 => " The firewall service couldn't remove the saved file of 1, so it may come back \
              when the service restarts."
            .to_string(),
        n => format!(
            " The firewall service couldn't remove the saved files of {n}, so they may come \
             back when the service restarts."
        ),
    };
    match &done.stopped {
        Some(stopped) if taken == 0 => Some(format!(
            "The rules were not removed: {}.",
            stopped.reason.trim_end_matches('.')
        )),
        Some(stopped) => Some(format!(
            "{of}, then it stopped: {}. The rest stay.{files}",
            stopped.reason.trim_end_matches('.')
        )),
        None if done.refused > 0 => Some(format!("{of}.{files}")),
        None => None,
    }
}

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
    /// Lists whose own rules are in but whose cleanup of an old kind is
    /// pending: a tick retries just that.
    cleanup_pending: Mutex<BTreeSet<String>>,
    /// How the last removal of leftover rules went, when it did not fully
    /// succeed, for the page to show under its button.
    leftover_outcome: Mutex<Option<String>>,
    /// Lists the daemon refused: how often in a row, and when a refresh tick
    /// may try again (issue #73).
    refusals: Mutex<HashMap<String, backoff::Refusal>>,
    /// [`AGGREGATE_MAX_HOSTS`], lowered in tests.
    aggregate_cap: u64,
    /// [`STORED_MAX_HOSTS`], lowered in tests.
    stored_cap: u64,
    /// Lists refused for room that a list removed since may have freed: the
    /// next tick tries them again. In memory only: after a restart they wait
    /// their normal interval.
    waiting_for_room: Mutex<BTreeSet<String>>,
}

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
            leftover_outcome: Mutex::new(None),
            cleanup_pending: Mutex::new(BTreeSet::new()),
            refusals: Mutex::new(HashMap::new()),
            aggregate_cap: AGGREGATE_MAX_HOSTS,
            stored_cap: STORED_MAX_HOSTS,
            waiting_for_room: Mutex::new(BTreeSet::new()),
        };
        manager.clear_lists_past_the_saved_limit_at_start();
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
        self.clear_lists_past_the_saved_limit_at_start();
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

    fn waiting_for_room(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.waiting_for_room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            .unwrap_or_else(|| self.default_enforcement(id))
    }

    /// Nothing recorded this run: the sink's reason, or, for a list whose
    /// saved hosts were cleared for the saved-hosts limit (in an earlier run,
    /// say), that; else not pushed yet.
    fn default_enforcement(&self, id: &str) -> Enforcement {
        if let Some(reason) = self.unavailable_reason() {
            return Enforcement::NotEnforced { reason };
        }
        let cleared = self
            .subscription(id)
            .is_some_and(|sub| sub.entry_count == 0 && refresh::refused_for_room(&sub));
        if cleared {
            return Enforcement::NotEnforced {
                reason: self.cleared_enforcement_reason(),
            };
        }
        Enforcement::Pending
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

    /// Why nothing manages the leftover rules: an unreadable store (they are
    /// probably lists still subscribed to) or no state directory. (A per-user
    /// service never gets here: it is on the legacy TCP connection, where
    /// nothing is offered until #35.) `None` when this bridge manages its
    /// rules.
    pub fn leftover_cause(&self) -> Option<&'static str> {
        if self.installs_rules() {
            return None;
        }
        Some(if self.load_error.is_some() {
            LEFTOVER_CAUSE_STORE_UNREADABLE
        } else {
            LEFTOVER_CAUSE_NO_STATE_DIR
        })
    }

    /// How the last removal went, while there are still leftovers.
    pub fn leftover_outcome(&self) -> Option<String> {
        self.leftover_count()?;
        self.leftover_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Delete the leftover rules, because the user asked. Does nothing while
    /// this bridge manages its rules. Tells GUIs what is left, and, if some
    /// stay, why.
    pub async fn remove_leftover_rules(&self) {
        if self.installs_rules() {
            warn!("asked to remove leftover blocklist rules while managing them; ignored");
            return;
        }
        let Some(leftover) = &self.leftover else {
            return;
        };
        let outcome = match leftover.remove_all().await {
            Ok(done) => {
                info!(
                    removed = done.removed,
                    refused = done.refused,
                    stopped = done.stopped.is_some(),
                    "removed leftover blocklist rules"
                );
                removal_note(&done)
            }
            Err(e) => {
                warn!(reason = %e.reason, "couldn't remove leftover blocklist rules");
                Some(format!(
                    "The rules were not removed: {}.",
                    e.reason.trim_end_matches('.')
                ))
            }
        };
        *self
            .leftover_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = outcome;
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
        if now.is_none() {
            // Nothing left, so nothing to say about how removing it went.
            *self
                .leftover_outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        let mut told = self.leftover_announced();
        if *told != now {
            *told = now;
            drop(told);
            let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        }
    }

    /// Record a sink outcome (the caller tells GUIs).
    fn record_install(&self, id: &str, outcome: &Result<(), NotInstalled>) {
        let mut pending = self
            .cleanup_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(outcome, Err(e) if e.cleanup_pending) {
            pending.insert(id.to_string());
        } else {
            pending.remove(id);
        }
        drop(pending);
        let enforcement = match outcome {
            Ok(()) => Enforcement::RuleInstalled { at: Utc::now() },
            Err(e) if e.daemon_unavailable || e.cleanup_pending => {
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
        // Room is measured against the lists before one, so only the lists
        // after this one gain any.
        let after: Vec<String> = self
            .subscriptions_in_order()
            .into_iter()
            .skip_while(|sub| sub.id != id)
            .skip(1)
            .map(|sub| sub.id)
            .collect();
        let owned = id.to_string();
        self.with_store(move |s| s.delete_subscription(&owned))
            .await?;
        self.cache().remove(id);
        self.order().retain(|other| other != id);
        self.enforcement_map().remove(id);
        self.forget_refusals(id);
        self.waiting_for_room().remove(id);
        self.cleanup_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        self.retry_lists_refused_for_room(&after);
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        if self.installs_rules() {
            if let Err(e) = self.rule_sink.release_blocklist_rules(id).await {
                warn!(%id, reason = %e.reason, "couldn't remove an unsubscribed list's rules");
            }
        }
        Ok(())
    }

    /// Ask the event pump for a page of `id`'s hosts (at most
    /// [`BLOCKLIST_ENTRIES_PAGE_MAX`]), to be sent with `request_id` to
    /// `reply`'s connection (everyone, with none).
    pub fn request_entries(
        &self,
        id: &str,
        offset: u64,
        limit: u32,
        request_id: Option<String>,
        reply: Option<ReplyTo>,
    ) {
        let _ = self.bus.send(BlocklistEvent::EntriesRequested {
            subscription_id: id.to_string(),
            offset,
            limit,
            request_id,
            reply,
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
}

#[path = "manager_backoff.rs"]
mod backoff;
#[path = "manager_reconcile.rs"]
mod reconcile;
#[path = "manager_refresh.rs"]
mod refresh;
