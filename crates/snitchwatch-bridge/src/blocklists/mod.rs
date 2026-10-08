//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.
//!
//! Production wiring (issue #45 PR A): one [`worker::BlocklistWorker`] runs
//! every subscribe, unsubscribe and scheduled refresh in order, so at most one
//! (bounded, https-only) fetch is in flight; [`spawn_event_pump`] turns
//! [`BlocklistEvent`]s into `ServerMessage`s. No daemon rules are installed
//! yet: the default [`NoopRuleSink`] reports every list as not enforced.

pub mod event_pump;
pub mod fetcher;
pub mod format;
pub mod materializer;
pub mod store;
pub mod worker;

pub use event_pump::spawn_event_pump;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::blocklists::fetcher::{
    validate_subscription_url, BlocklistFetch, FetchOutcome, HttpsFetcher,
};
use crate::blocklists::materializer::{materialize_batch, MaterializedRule};
use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};
use crate::ws_messages::StorageStatus;

/// Events emitted whenever blocklist state changes. [`spawn_event_pump`]
/// rebroadcasts them as `SetBlocklists` / `SetBlocklistEntries` / … over the WS.
#[derive(Debug, Clone)]
pub enum BlocklistEvent {
    SubscriptionsChanged,
    EntriesChanged {
        subscription_id: String,
    },
    StatusChanged {
        subscription_id: String,
    },
    /// A subscribe request was refused before anything was stored (a bad URL,
    /// or the worker queue was full). Shown to the user, never persisted.
    SubscriptionRejected {
        url: String,
        reason: String,
    },
}

/// Whether a subscription's hosts are enforced by the daemon. Kept in memory
/// only; [`FetchStatus`] separately records the download result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforcement {
    /// Not downloaded or pushed to a sink yet.
    Pending,
    /// The sink accepted the rules: the daemon replied OK and (PR B) the list
    /// file was written. Shown as "Rule installed", not "Enforced": the daemon
    /// may still load 0 entries.
    RuleInstalled {
        at: DateTime<Utc>,
    },
    NotEnforced {
        reason: String,
    },
}

/// Why every list is unenforced until PR B wires a daemon sink.
pub const NO_RULE_SINK_REASON: &str = "no rule sink yet";
/// Enforcement reason for a subscription whose first download failed.
pub const NOT_DOWNLOADED_REASON: &str = "list not downloaded";
/// Sink that receives materialized deny rules after a successful blocklist
/// refresh. The default implementation is [`NoopRuleSink`]; replace it with
/// [`BlocklistsManager::with_rule_sink`] to wire in the real opensnitchd writer.
///
/// ## Replace contract (migration-critical)
///
/// `replace_blocklist_rules` has *replace* — not merely *add* — semantics: the
/// daemon's set of rules for `list_id` must end up **exactly** `rules`. An
/// implementation must therefore delete every existing rule whose name starts
/// with any prefix in
/// [`materializer::owned_blocklist_rule_name_prefixes`](crate::blocklists::materializer::owned_blocklist_rule_name_prefixes)
/// (that set spans both the current `"z00-blocklist:"` band and the legacy
/// `"900-blocklist:"` band) before installing `rules`. That is what migrates a
/// daemon off the old band: because the band move changes each rule's *name*,
/// the new rules do not overwrite the old ones by name, so the stale
/// old-prefix denies must be purged explicitly. See
/// [`materializer`](crate::blocklists::materializer)'s "Legacy band migration"
/// note. The `refresh_removes_legacy_band_rules` test exercises a sink that
/// honors this contract end-to-end.
// clippy 1.99's `double_must_use` fires on async_trait's generated
// `#[must_use]` methods (they return an already-must_use boxed future).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RuleSink: Send + Sync + 'static {
    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()>;
}

/// Sink used when no real sink has been wired in. It installs nothing, so it
/// fails every push with [`NO_RULE_SINK_REASON`]: the manager must never
/// report a list as installed when no rule reached the daemon.
pub struct NoopRuleSink;

#[async_trait]
impl RuleSink for NoopRuleSink {
    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()> {
        anyhow::bail!(NO_RULE_SINK_REASON)
    }
}

pub struct BlocklistsManager {
    store: Arc<BlocklistStore>,
    bus: broadcast::Sender<BlocklistEvent>,
    fetcher: Arc<dyn BlocklistFetch>,
    rule_sink: Arc<dyn RuleSink>,
    storage: StorageStatus,
    enforcement: Mutex<HashMap<String, Enforcement>>,
}

impl BlocklistsManager {
    pub fn new(store: Arc<BlocklistStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        Self {
            store,
            bus,
            fetcher: Arc::new(HttpsFetcher::new()),
            rule_sink: Arc::new(NoopRuleSink),
            storage: StorageStatus {
                persistent: false,
                reason: None,
            },
            enforcement: Mutex::new(HashMap::new()),
        }
    }

    /// Replace the default no-op rule sink with a real implementation.
    pub fn with_rule_sink(mut self, sink: Arc<dyn RuleSink>) -> Self {
        self.rule_sink = sink;
        self
    }

    /// Replace the default [`HttpsFetcher`]. Tests only: production never
    /// sets a fetcher, so it has no non-https fetch path.
    pub fn with_fetcher(mut self, fetcher: Arc<dyn BlocklistFetch>) -> Self {
        self.fetcher = fetcher;
        self
    }

    /// Record whether [`store`](Self::store) outlives the bridge process. Sent
    /// to GUIs with every `SetBlocklists`.
    pub fn with_storage_status(mut self, storage: StorageStatus) -> Self {
        self.storage = storage;
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

    /// The subscription's current enforcement state ([`Enforcement::Pending`]
    /// until its first refresh).
    pub fn enforcement(&self, id: &str) -> Enforcement {
        self.enforcement_map()
            .get(id)
            .cloned()
            .unwrap_or(Enforcement::Pending)
    }

    fn enforcement_map(&self) -> MutexGuard<'_, HashMap<String, Enforcement>> {
        self.enforcement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Add a subscription record (does not fetch). Use [`refresh_now`] to pull.
    /// Refuses (and stores nothing for) a URL that
    /// [`validate_subscription_url`] rejects.
    pub async fn add_subscription(&self, url: &str) -> anyhow::Result<String> {
        let url = validate_subscription_url(url)
            .map_err(|reason| anyhow::anyhow!("refused blocklist URL: {reason}"))?;
        let url = url.as_str();
        let id = derive_id(url);
        let display_name = derive_display_name(url);
        let sub = Subscription {
            id: id.clone(),
            url: url.to_string(),
            display_name,
            format_hint: None,
            refresh_interval_secs: 86_400,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Pending,
            entry_count: 0,
        };
        self.store.upsert_subscription(&sub)?;
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        Ok(id)
    }

    /// Tell GUIs a subscribe request was refused. Nothing is stored.
    pub fn reject_subscription(&self, url: &str, reason: &str) {
        let _ = self.bus.send(BlocklistEvent::SubscriptionRejected {
            url: url.to_string(),
            reason: reason.to_string(),
        });
    }

    pub async fn remove_subscription(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_subscription(id)?;
        self.enforcement_map().remove(id);
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        Ok(())
    }

    /// Pull a subscription synchronously and update the store + bus accordingly.
    pub async fn refresh_now(&self, id: &str) -> anyhow::Result<FetchStatus> {
        let Some(mut sub) = self.store.get_subscription(id)? else {
            anyhow::bail!("unknown subscription: {id}");
        };
        let outcome = self.fetcher.fetch(&sub.url).await;
        let new_status = match outcome {
            FetchOutcome::Ok { hosts, .. } => {
                let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
                self.store.replace_entries(&sub.id, &host_refs)?;
                sub.entry_count = host_refs.len() as i64;
                sub.last_fetched_at = Some(Utc::now());
                sub.last_fetch_status = FetchStatus::Ok;
                self.store.upsert_subscription(&sub)?;
                let materialized = materialize_batch(&sub.id, &hosts);
                let enforcement = match self
                    .rule_sink
                    .replace_blocklist_rules(&sub.id, materialized)
                    .await
                {
                    Ok(()) => Enforcement::RuleInstalled { at: Utc::now() },
                    Err(e) => {
                        warn!(id = %sub.id, error = %e, "rule sink push failed; entries cached but not enforced");
                        Enforcement::NotEnforced {
                            reason: e.to_string(),
                        }
                    }
                };
                self.enforcement_map().insert(sub.id.clone(), enforcement);
                let _ = self.bus.send(BlocklistEvent::EntriesChanged {
                    subscription_id: sub.id.clone(),
                });
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: sub.id.clone(),
                });
                info!(id = %sub.id, count = host_refs.len(), "blocklist refreshed");
                FetchStatus::Ok
            }
            FetchOutcome::Failed { reason } => {
                sub.last_fetch_status = FetchStatus::Failed {
                    reason: reason.clone(),
                };
                self.store.upsert_subscription(&sub)?;
                // A previous push stays in effect; only a never-pushed list
                // changes state.
                self.enforcement_map()
                    .entry(sub.id.clone())
                    .or_insert_with(|| Enforcement::NotEnforced {
                        reason: NOT_DOWNLOADED_REASON.to_string(),
                    });
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: sub.id.clone(),
                });
                warn!(id = %sub.id, %reason, "blocklist refresh failed; cache preserved");
                FetchStatus::Failed { reason }
            }
        };
        Ok(new_status)
    }

    /// Refresh every subscription whose interval has elapsed (or that was never
    /// fetched), one at a time. Run as the worker's `RefreshDue` job.
    pub async fn refresh_due(&self) {
        let due = match self.due_subscriptions() {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "blocklist scheduler: store read failed");
                return;
            }
        };
        for id in due {
            if let Err(e) = self.refresh_now(&id).await {
                warn!(%id, error = %e, "scheduled refresh failed");
            }
        }
    }

    fn due_subscriptions(&self) -> Result<Vec<String>, store::StoreError> {
        let now = Utc::now();
        let subs = self.store.list_subscriptions()?;
        Ok(subs
            .into_iter()
            .filter(|s| match s.last_fetched_at {
                None => true,
                Some(t) => {
                    let elapsed = (now - t).num_seconds();
                    elapsed >= s.refresh_interval_secs
                }
            })
            .map(|s| s.id)
            .collect())
    }
}

/// Longest sanitized stem kept in an id. Ids become rule names and (PR B) list
/// directory names, so a 2 KiB URL must not produce a 2 KiB id.
const MAX_ID_STEM_CHARS: usize = 64;

/// `<sanitized stem>-<8 hex of SHA-256(url)>`. The hash keeps two URLs that
/// end in the same file name (`…/hosts`) apart; the result only ever contains
/// `[A-Za-z0-9_-]`.
pub(crate) fn derive_id(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(url.as_bytes());
    let hash: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{hash}", sanitized_stem(url))
}

pub(crate) fn derive_display_name(url: &str) -> String {
    sanitized_stem(url).replace('_', " ")
}

/// The URL's last path segment without query or `.txt`, with every character
/// outside `[A-Za-z0-9_-]` replaced by `_`, capped, or `list` if empty.
fn sanitized_stem(url: &str) -> String {
    let stem = url
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".txt");
    let cleaned: String = stem
        .chars()
        .take(MAX_ID_STEM_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "list".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
pub mod test_helpers {
    use crate::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
    use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};
    use crate::blocklists::BlocklistsManager;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const FIXTURE_PREFIX: &str = "https://fixtures.invalid/";
    const FIXTURE_MAX_BYTES: u64 = 1024 * 1024;

    /// An https URL the [`FixtureFetcher`] serves from
    /// `tests/fixtures/blocklists/<name>`.
    pub fn fixture_url(name: &str) -> String {
        format!("{FIXTURE_PREFIX}{name}")
    }

    /// Test-only [`BlocklistFetch`]: reads `tests/fixtures/blocklists/<name>`
    /// for [`fixture_url`] URLs (size-capped), fails everything else. Counts
    /// calls. Production has no file-reading fetch path; this lives here.
    #[derive(Default)]
    pub struct FixtureFetcher {
        pub calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl BlocklistFetch for FixtureFetcher {
        async fn fetch(&self, url: &str) -> FetchOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let Some(name) = url.strip_prefix(FIXTURE_PREFIX) else {
                return FetchOutcome::Failed {
                    reason: format!("not a fixture URL: {url}"),
                };
            };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/blocklists")
                .join(name);
            match std::fs::metadata(&path) {
                Ok(m) if m.len() <= FIXTURE_MAX_BYTES => {}
                Ok(m) => {
                    return FetchOutcome::Failed {
                        reason: format!("fixture too large: {} bytes", m.len()),
                    }
                }
                Err(e) => {
                    return FetchOutcome::Failed {
                        reason: format!("fixture {name}: {e}"),
                    }
                }
            }
            match std::fs::read_to_string(&path) {
                Ok(body) => process_body(&body),
                Err(e) => FetchOutcome::Failed {
                    reason: format!("fixture {name}: {e}"),
                },
            }
        }
    }

    pub fn seeded_manager(seeds: &[(&str, usize)]) -> BlocklistsManager {
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        for (id, n_entries) in seeds {
            store
                .upsert_subscription(&Subscription {
                    id: (*id).to_string(),
                    url: format!("https://example.invalid/{id}.txt"),
                    display_name: (*id).to_string(),
                    format_hint: None,
                    refresh_interval_secs: 86_400,
                    last_fetched_at: None,
                    last_fetch_status: FetchStatus::Ok,
                    entry_count: *n_entries as i64,
                })
                .unwrap();
            let hosts: Vec<String> = (0..*n_entries)
                .map(|i| format!("host{i}.{id}.example"))
                .collect();
            let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
            store.replace_entries(id, &host_refs).unwrap();
        }
        BlocklistsManager::new(store)
    }
}

#[cfg(test)]
mod tests;
