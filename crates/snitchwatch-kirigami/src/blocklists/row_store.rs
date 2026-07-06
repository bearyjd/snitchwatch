//! Pure, Qt-free stores for the two-level Blocklists view (Task 9).
//!
//! This re-homes `web/js/blocklists.js`'s `setBlocklists` /
//! `setBlocklistEntries` / `setBlocklistStatus` handlers into Rust. The bridge
//! emits the same typed [`ServerMessage`] variants; these stores fold them into
//! ordered lists the cxx-qt wrappers expose to QML.
//!
//! Blocklist state changes far less often than the connection feed (a
//! subscription refresh is a whole-list replace, not a hot per-row stream), so
//! — unlike the connections `RowStore` — these stores report changes as a
//! single "did anything change" boolean and the model wrapper brackets each
//! with a `beginResetModel`/`endResetModel`. That is always view-correct and
//! avoids incremental-range bookkeeping that would buy nothing here.

use snitchwatch_bridge::ws_messages::{BlocklistSummary, ServerMessage};

/// Ordered list of blocklist subscriptions (the master list).
#[derive(Debug, Default)]
pub struct SubscriptionsStore {
    subs: Vec<BlocklistSummary>,
}

impl SubscriptionsStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.subs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    pub fn subscriptions(&self) -> &[BlocklistSummary] {
        &self.subs
    }

    pub fn row(&self, index: usize) -> Option<&BlocklistSummary> {
        self.subs.get(index)
    }

    /// Apply one bridge message. Returns `true` if the subscription list
    /// changed (the model wrapper resets on `true`).
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetBlocklists { blocklists } => {
                self.subs = blocklists.clone();
                true
            }
            ServerMessage::SetBlocklistDetails { details } => self.upsert(details.clone()),
            ServerMessage::SetBlocklistStatus {
                subscription_id,
                status,
                last_failure_reason,
            } => self.update_status(subscription_id, status, last_failure_reason.clone()),
            _ => false,
        }
    }

    fn upsert(&mut self, details: BlocklistSummary) -> bool {
        match self.subs.iter_mut().find(|s| s.id == details.id) {
            Some(existing) => {
                *existing = details;
            }
            None => self.subs.push(details),
        }
        true
    }

    fn update_status(
        &mut self,
        id: &str,
        status: &str,
        last_failure_reason: Option<String>,
    ) -> bool {
        match self.subs.iter_mut().find(|s| s.id == id) {
            Some(sub) => {
                sub.status = status.to_string();
                sub.last_failure_reason = last_failure_reason;
                true
            }
            None => false,
        }
    }
}

/// The entries (hosts) of the currently displayed subscription (the detail
/// list). Only the entries for `subscription_id` are held at a time — the WS
/// `setBlocklistEntries` message carries one subscription's entries per send.
#[derive(Debug, Default)]
pub struct EntriesStore {
    subscription_id: String,
    hosts: Vec<String>,
}

impl EntriesStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    pub fn subscription_id(&self) -> &str {
        &self.subscription_id
    }

    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    pub fn host(&self, index: usize) -> Option<&str> {
        self.hosts.get(index).map(String::as_str)
    }

    /// Apply one bridge message. Returns `true` if the entry list changed.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetBlocklistEntries {
                subscription_id,
                entries,
            } => {
                self.subscription_id = subscription_id.clone();
                self.hosts = entries.iter().map(|e| e.host.clone()).collect();
                true
            }
            _ => false,
        }
    }

    /// Clear the detail list (e.g. when the selection is cleared).
    pub fn clear(&mut self) -> bool {
        if self.hosts.is_empty() && self.subscription_id.is_empty() {
            return false;
        }
        self.subscription_id.clear();
        self.hosts.clear();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::BlocklistEntry;

    fn summary(id: &str, status: &str, count: i64) -> BlocklistSummary {
        BlocklistSummary {
            id: id.to_string(),
            display_name: format!("{id} list"),
            url: format!("https://example.invalid/{id}.txt"),
            entry_count: count,
            status: status.to_string(),
            last_updated_iso8601: None,
            last_failure_reason: None,
        }
    }

    fn ids(store: &SubscriptionsStore) -> Vec<String> {
        store.subscriptions().iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn set_blocklists_replaces_the_list() {
        let mut s = SubscriptionsStore::new();
        assert!(s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("a", "ok", 10), summary("b", "ok", 20)],
        }));
        assert_eq!(ids(&s), vec!["a", "b"]);
        // A second SetBlocklists replaces wholesale.
        assert!(s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("c", "ok", 5)],
        }));
        assert_eq!(ids(&s), vec!["c"]);
    }

    #[test]
    fn set_details_upserts_by_id() {
        let mut s = SubscriptionsStore::new();
        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("a", "ok", 10)],
        });
        // Update existing "a".
        assert!(s.apply(&ServerMessage::SetBlocklistDetails {
            details: summary("a", "ok", 99),
        }));
        assert_eq!(s.row(0).unwrap().entry_count, 99);
        // Insert new "z".
        assert!(s.apply(&ServerMessage::SetBlocklistDetails {
            details: summary("z", "ok", 1),
        }));
        assert_eq!(ids(&s), vec!["a", "z"]);
    }

    #[test]
    fn set_status_updates_matching_subscription() {
        let mut s = SubscriptionsStore::new();
        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("a", "ok", 10)],
        });
        assert!(s.apply(&ServerMessage::SetBlocklistStatus {
            subscription_id: "a".to_string(),
            status: "failed".to_string(),
            last_failure_reason: Some("dns error".to_string()),
        }));
        assert_eq!(s.row(0).unwrap().status, "failed");
        assert_eq!(
            s.row(0).unwrap().last_failure_reason.as_deref(),
            Some("dns error")
        );
    }

    #[test]
    fn set_status_for_unknown_id_is_noop() {
        let mut s = SubscriptionsStore::new();
        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("a", "ok", 10)],
        });
        assert!(!s.apply(&ServerMessage::SetBlocklistStatus {
            subscription_id: "nope".to_string(),
            status: "failed".to_string(),
            last_failure_reason: None,
        }));
    }

    #[test]
    fn unrelated_message_does_not_change_subscriptions() {
        let mut s = SubscriptionsStore::new();
        assert!(!s.apply(&ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn entries_store_holds_one_subscription_at_a_time() {
        let mut e = EntriesStore::new();
        assert!(e.apply(&ServerMessage::SetBlocklistEntries {
            subscription_id: "a".to_string(),
            entries: vec![
                BlocklistEntry {
                    host: "ads.example".to_string(),
                },
                BlocklistEntry {
                    host: "tracker.example".to_string(),
                },
            ],
        }));
        assert_eq!(e.subscription_id(), "a");
        assert_eq!(e.len(), 2);
        assert_eq!(e.host(0), Some("ads.example"));

        // Switching subscription replaces the detail list.
        assert!(e.apply(&ServerMessage::SetBlocklistEntries {
            subscription_id: "b".to_string(),
            entries: vec![BlocklistEntry {
                host: "b1.example".to_string(),
            }],
        }));
        assert_eq!(e.subscription_id(), "b");
        assert_eq!(e.hosts(), &["b1.example".to_string()]);
    }

    #[test]
    fn entries_clear_resets_and_is_idempotent() {
        let mut e = EntriesStore::new();
        e.apply(&ServerMessage::SetBlocklistEntries {
            subscription_id: "a".to_string(),
            entries: vec![BlocklistEntry {
                host: "x.example".to_string(),
            }],
        });
        assert!(e.clear());
        assert!(e.is_empty());
        assert_eq!(e.subscription_id(), "");
        // Second clear is a no-op.
        assert!(!e.clear());
    }

    #[test]
    fn entries_store_ignores_unrelated_messages() {
        let mut e = EntriesStore::new();
        assert!(!e.apply(&ServerMessage::ClearConnectionRows));
    }
}
