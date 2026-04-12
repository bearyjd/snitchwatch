//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.

pub mod fetcher;
pub mod format;
pub mod materializer;
pub mod store;

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::blocklists::fetcher::{build_client, fetch, FetchOutcome};
use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};

/// Events emitted whenever blocklist state changes. The translator subscribes
/// and rebroadcasts as `SetBlocklists` / `SetBlocklistEntries` over the WS.
#[derive(Debug, Clone)]
pub enum BlocklistEvent {
    SubscriptionsChanged,
    EntriesChanged { subscription_id: String },
    StatusChanged { subscription_id: String },
}

pub struct BlocklistsManager {
    store: Arc<BlocklistStore>,
    bus: broadcast::Sender<BlocklistEvent>,
    client: reqwest::Client,
}

impl BlocklistsManager {
    pub fn new(store: Arc<BlocklistStore>) -> Self {
        let (bus, _) = broadcast::channel(64);
        Self {
            store,
            bus,
            client: build_client(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BlocklistEvent> {
        self.bus.subscribe()
    }

    pub fn store(&self) -> &Arc<BlocklistStore> {
        &self.store
    }

    /// Add a subscription record (does not fetch). Use [`refresh_now`] to pull.
    pub async fn add_subscription(&self, url: &str) -> anyhow::Result<String> {
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

    pub async fn remove_subscription(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_subscription(id)?;
        let _ = self.bus.send(BlocklistEvent::SubscriptionsChanged);
        Ok(())
    }

    /// Pull a subscription synchronously and update the store + bus accordingly.
    pub async fn refresh_now(&self, id: &str) -> anyhow::Result<FetchStatus> {
        let Some(mut sub) = self.store.get_subscription(id)? else {
            anyhow::bail!("unknown subscription: {id}");
        };
        let outcome = fetch(&self.client, &sub.url).await;
        let new_status = match outcome {
            FetchOutcome::Ok { hosts, .. } => {
                let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
                self.store.replace_entries(&sub.id, &host_refs)?;
                sub.entry_count = host_refs.len() as i64;
                sub.last_fetched_at = Some(Utc::now());
                sub.last_fetch_status = FetchStatus::Ok;
                self.store.upsert_subscription(&sub)?;
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
                sub.last_fetch_status = FetchStatus::Failed { reason: reason.clone() };
                self.store.upsert_subscription(&sub)?;
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: sub.id.clone(),
                });
                warn!(id = %sub.id, %reason, "blocklist refresh failed; cache preserved");
                FetchStatus::Failed { reason }
            }
        };
        Ok(new_status)
    }
}

fn derive_id(url: &str) -> String {
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
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        format!("list-{:x}", url.len())
    } else {
        cleaned
    }
}

fn derive_display_name(url: &str) -> String {
    derive_id(url).replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> BlocklistsManager {
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        BlocklistsManager::new(store)
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
        assert_eq!(id, "list");
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

    #[test]
    fn derive_id_handles_query_strings_and_special_chars() {
        assert_eq!(derive_id("https://x.example/hosts.txt?branch=main"), "hosts");
        assert_eq!(derive_id("https://x.example/StevenBlack/hosts"), "hosts");
        assert_eq!(derive_id("https://x.example/path/with%20space"), "with_20space");
    }
}
