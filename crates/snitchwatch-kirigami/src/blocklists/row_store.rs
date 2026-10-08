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

use snitchwatch_bridge::ws_messages::{
    BlocklistSummary, ServerMessage, StorageStatus, ENFORCEMENT_PENDING, ENFORCEMENT_RULE_INSTALLED,
};

/// Ordered list of blocklist subscriptions (the master list).
#[derive(Debug, Default)]
pub struct SubscriptionsStore {
    subs: Vec<BlocklistSummary>,
    /// From the last `SetBlocklists`; `None` until one arrives or from an
    /// older bridge, which kept subscriptions in memory only.
    storage: Option<StorageStatus>,
}

/// The download result as shown to the user. `status` only says whether the
/// list downloaded, so the label must not read as "working".
pub fn status_label(status: &str) -> String {
    match status {
        "ok" => "Downloaded".to_string(),
        "pending" => "Downloading".to_string(),
        "failed" => "Download failed".to_string(),
        "refused" => "Not downloaded".to_string(),
        other => other.to_string(),
    }
}

/// Whether the list blocks anything (issue #45). Only an installed daemon
/// rule earns more than "not enforced", and even then the daemon may have
/// loaded 0 entries, so it says "Rule installed", never "Enforced".
pub fn enforcement_label(sub: &BlocklistSummary) -> &'static str {
    match sub.enforcement.as_str() {
        ENFORCEMENT_RULE_INSTALLED => "Rule installed",
        ENFORCEMENT_PENDING => "Not enforced yet",
        _ => "Not enforced",
    }
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

    /// True while some list isn't reported "rule installed" (issue #45): the
    /// page warns until every list's rule was accepted by the daemon. An
    /// older bridge, which sends no enforcement, counts as not enforced.
    pub fn any_not_enforced(&self) -> bool {
        self.subs
            .iter()
            .any(|sub| sub.enforcement != ENFORCEMENT_RULE_INSTALLED)
    }

    /// True only when the bridge said its subscriptions survive a restart.
    pub fn storage_persistent(&self) -> bool {
        self.storage.as_ref().is_some_and(|s| s.persistent)
    }

    /// Why the bridge couldn't persist subscriptions, or "".
    pub fn storage_reason(&self) -> &str {
        self.storage
            .as_ref()
            .and_then(|s| s.reason.as_deref())
            .unwrap_or("")
    }

    /// Apply one bridge message. Returns `true` if the subscription list
    /// changed (the model wrapper resets on `true`).
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetBlocklists {
                blocklists,
                storage,
            } => {
                self.subs = blocklists.clone();
                self.storage = storage.clone();
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
/// list). Only the entries for `subscription_id` are held at a time. The
/// bridge sends them a page at a time, on request (issue #45); `total` is the
/// subscription's full entry count.
#[derive(Debug, Default)]
pub struct EntriesStore {
    subscription_id: String,
    hosts: Vec<String>,
    total: u64,
    /// The list this GUI asked for. Pages are broadcast to every GUI, so a
    /// page for any other list is someone else's and is ignored.
    wanted: Option<String>,
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

    /// The subscription's full entry count (not just the loaded pages).
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Whether more pages can be requested.
    pub fn has_more(&self) -> bool {
        (self.hosts.len() as u64) < self.total
    }

    /// Record that this GUI asked for `id`'s entries. Returns `true` if the
    /// shown list changed (a different list's entries are dropped).
    pub fn expect(&mut self, id: &str) -> bool {
        if self.wanted.as_deref() == Some(id) {
            return false;
        }
        self.wanted = Some(id.to_string());
        if self.subscription_id == id {
            return false;
        }
        self.subscription_id.clear();
        self.hosts.clear();
        self.total = 0;
        true
    }

    /// Apply one bridge message. Returns `true` if the entry list changed.
    /// Only pages of the list this GUI asked for ([`expect`](Self::expect))
    /// count: a page at offset 0 replaces the list, the next page appends,
    /// any other page is ignored.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetBlocklistEntries {
                subscription_id,
                entries,
                offset,
                total,
            } => {
                if self.wanted.as_deref() != Some(subscription_id.as_str()) {
                    return false;
                }
                let hosts = entries.iter().map(|e| e.host.clone());
                if *offset == 0 {
                    self.subscription_id = subscription_id.clone();
                    self.hosts = hosts.collect();
                } else if *subscription_id == self.subscription_id
                    && *offset == self.hosts.len() as u64
                {
                    self.hosts.extend(hosts);
                } else {
                    return false;
                }
                self.total = *total;
                true
            }
            _ => false,
        }
    }

    /// Clear the detail list (e.g. when the selection is cleared).
    pub fn clear(&mut self) -> bool {
        if self.hosts.is_empty() && self.subscription_id.is_empty() {
            self.wanted = None;
            return false;
        }
        self.subscription_id.clear();
        self.hosts.clear();
        self.total = 0;
        self.wanted = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::{
        BlocklistEntry, StorageStatus, ENFORCEMENT_NOT_ENFORCED, ENFORCEMENT_PENDING,
        ENFORCEMENT_RULE_INSTALLED,
    };

    fn summary(id: &str, status: &str, count: i64) -> BlocklistSummary {
        BlocklistSummary {
            id: id.to_string(),
            display_name: format!("{id} list"),
            url: format!("https://example.invalid/{id}.txt"),
            entry_count: count,
            status: status.to_string(),
            last_updated_iso8601: None,
            last_failure_reason: None,
            enforcement: String::new(),
            enforcement_reason: None,
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
            storage: None,
        }));
        assert_eq!(ids(&s), vec!["a", "b"]);
        // A second SetBlocklists replaces wholesale.
        assert!(s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("c", "ok", 5)],
            storage: None,
        }));
        assert_eq!(ids(&s), vec!["c"]);
    }

    #[test]
    fn set_details_upserts_by_id() {
        let mut s = SubscriptionsStore::new();
        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![summary("a", "ok", 10)],
            storage: None,
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
            storage: None,
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
            storage: None,
        });
        assert!(!s.apply(&ServerMessage::SetBlocklistStatus {
            subscription_id: "nope".to_string(),
            status: "failed".to_string(),
            last_failure_reason: None,
        }));
    }

    fn persistent() -> Option<StorageStatus> {
        Some(StorageStatus {
            persistent: true,
            reason: None,
        })
    }

    /// Issue #45: the page drops "lost on restart" only for a bridge that
    /// says its subscriptions persist. Unknown (no message yet, or an older
    /// bridge) means not persistent.
    #[test]
    fn storage_defaults_to_not_persistent_and_follows_set_blocklists() {
        let mut s = SubscriptionsStore::new();
        assert!(!s.storage_persistent());
        assert_eq!(s.storage_reason(), "");

        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![],
            storage: persistent(),
        });
        assert!(s.storage_persistent());

        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![],
            storage: Some(StorageStatus {
                persistent: false,
                reason: Some("blocklist store: disk I/O error".into()),
            }),
        });
        assert!(!s.storage_persistent());
        assert_eq!(s.storage_reason(), "blocklist store: disk I/O error");

        // An older bridge sends no storage at all.
        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![],
            storage: None,
        });
        assert!(!s.storage_persistent());
        assert_eq!(s.storage_reason(), "");
    }

    /// A downloaded list is labelled as downloaded, never as working or
    /// enforced: PR A installs no daemon rule.
    #[test]
    fn labels_never_claim_enforcement_without_an_installed_rule() {
        assert_eq!(status_label("ok"), "Downloaded");
        assert_eq!(status_label("pending"), "Downloading");
        assert_eq!(status_label("failed"), "Download failed");

        let mut row = summary("a", "ok", 10);
        for (enforcement, label) in [
            (ENFORCEMENT_NOT_ENFORCED, "Not enforced"),
            (ENFORCEMENT_PENDING, "Not enforced yet"),
            ("", "Not enforced"),
            ("something-new", "Not enforced"),
            (ENFORCEMENT_RULE_INSTALLED, "Rule installed"),
        ] {
            row.enforcement = enforcement.to_string();
            assert_eq!(enforcement_label(&row), label, "{enforcement:?}");
        }
    }

    /// Issue #45 PR B: the page-level warning shows while any list isn't
    /// reported "rule installed", including an older bridge's (no field).
    #[test]
    fn any_not_enforced_is_true_until_every_list_has_an_installed_rule() {
        let mut s = SubscriptionsStore::new();
        assert!(!s.any_not_enforced(), "no lists, nothing to warn about");
        let mut installed = summary("a", "ok", 10);
        installed.enforcement = ENFORCEMENT_RULE_INSTALLED.to_string();
        for other in [
            ENFORCEMENT_NOT_ENFORCED,
            ENFORCEMENT_PENDING,
            "",
            "something-new",
        ] {
            let mut row = summary("b", "ok", 10);
            row.enforcement = other.to_string();
            s.apply(&ServerMessage::SetBlocklists {
                blocklists: vec![installed.clone(), row],
                storage: None,
            });
            assert!(s.any_not_enforced(), "{other:?}");
        }
        s.apply(&ServerMessage::SetBlocklists {
            blocklists: vec![installed],
            storage: None,
        });
        assert!(!s.any_not_enforced());
    }

    #[test]
    fn unrelated_message_does_not_change_subscriptions() {
        let mut s = SubscriptionsStore::new();
        assert!(!s.apply(&ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn entries_store_holds_one_subscription_at_a_time() {
        let mut e = EntriesStore::new();
        e.expect("a");
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
            offset: 0,
            total: 2,
        }));
        assert_eq!(e.subscription_id(), "a");
        assert_eq!(e.len(), 2);
        assert_eq!(e.host(0), Some("ads.example"));

        // Switching subscription replaces the detail list.
        e.expect("b");
        assert!(e.apply(&ServerMessage::SetBlocklistEntries {
            subscription_id: "b".to_string(),
            entries: vec![BlocklistEntry {
                host: "b1.example".to_string(),
            }],
            offset: 0,
            total: 1,
        }));
        assert_eq!(e.subscription_id(), "b");
        assert_eq!(e.hosts(), &["b1.example".to_string()]);
    }

    #[test]
    fn entries_clear_resets_and_is_idempotent() {
        let mut e = EntriesStore::new();
        e.expect("a");
        e.apply(&ServerMessage::SetBlocklistEntries {
            subscription_id: "a".to_string(),
            entries: vec![BlocklistEntry {
                host: "x.example".to_string(),
            }],
            offset: 0,
            total: 1,
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

    fn page(id: &str, hosts: &[&str], offset: u64, total: u64) -> ServerMessage {
        ServerMessage::SetBlocklistEntries {
            subscription_id: id.to_string(),
            entries: hosts
                .iter()
                .map(|h| BlocklistEntry {
                    host: h.to_string(),
                })
                .collect(),
            offset,
            total,
        }
    }

    /// Issue #45 (S2): entries arrive a page at a time; the next page of the
    /// same list appends, anything out of sequence is ignored.
    #[test]
    fn entry_pages_append_in_sequence() {
        let mut e = EntriesStore::new();
        e.expect("a");
        assert!(e.apply(&page("a", &["1.x", "2.x"], 0, 5)));
        assert_eq!(e.total(), 5);
        assert!(e.has_more());
        assert!(e.apply(&page("a", &["3.x", "4.x"], 2, 5)));
        assert_eq!(e.len(), 4);
        // A stale or duplicate page is ignored.
        assert!(!e.apply(&page("a", &["3.x", "4.x"], 2, 5)));
        assert!(!e.apply(&page("b", &["9.x"], 1, 5)));
        assert!(e.apply(&page("a", &["5.x"], 4, 5)));
        assert!(!e.has_more());
        assert_eq!(e.hosts().len(), 5);
        // A first page of the next list asked for replaces what was shown.
        e.expect("b");
        assert!(e.apply(&page("b", &["b.x"], 0, 1)));
        assert_eq!(e.subscription_id(), "b");
        assert_eq!(e.hosts(), &["b.x".to_string()]);
    }

    /// Entry pages are broadcast to every GUI: a page another GUI asked for
    /// must not replace this inspector's list (or blank it).
    #[test]
    fn pages_for_a_list_this_gui_did_not_ask_for_are_ignored() {
        let mut e = EntriesStore::new();
        assert!(
            !e.apply(&page("a", &["1.x"], 0, 1)),
            "nothing was asked for yet"
        );
        assert!(e.is_empty());
        e.expect("a");
        assert!(e.apply(&page("a", &["1.x"], 0, 1)));
        assert!(!e.apply(&page("other", &["o.x"], 0, 1)));
        assert_eq!(e.subscription_id(), "a");
        assert_eq!(e.hosts(), &["1.x".to_string()]);
        // Asking for another list drops the old one until its page arrives.
        assert!(e.expect("b"));
        assert!(e.is_empty());
        assert!(!e.expect("b"), "asking again changes nothing");
    }

    /// L10: a refused URL never reads as a failed download.
    #[test]
    fn a_refused_url_is_labelled_not_downloaded() {
        assert_eq!(status_label("refused"), "Not downloaded");
    }
}
