//! The entries (hosts) of the blocklist being inspected: a Qt-free store the
//! `BlocklistEntriesModel` wraps.

use snitchwatch_bridge::ws_messages::ServerMessage;
use std::sync::OnceLock;

/// Names this GUI process in its `RequestBlocklistEntries` (issue #67). The
/// bridge sends every page to every GUI with the request id it answers, so
/// this GUI keeps only pages that carry this id. Random per process: two
/// GUIs never share one, even in separate sandboxes with the same pid.
pub fn client_request_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let token = snitchwatch_bridge::auth::Token::generate();
        token.as_str().chars().take(16).collect()
    })
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
    /// When the held pages' list was downloaded (the bridge's
    /// `lastUpdatedIso8601` on each page). A later page that says otherwise
    /// comes from different contents.
    download: Option<String>,
    /// The list to ask for again from the start, because its contents changed
    /// between two pages. Taken by [`take_restart`](Self::take_restart).
    restart: Option<String>,
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
        self.restart = None;
        if self.subscription_id == id {
            return false;
        }
        self.subscription_id.clear();
        self.hosts.clear();
        self.total = 0;
        self.download = None;
        true
    }

    /// Apply one bridge message. Returns `true` if the entry list changed.
    /// Only pages this GUI asked for count: those for the list it
    /// [`expect`](Self::expect)s, and (from a bridge that tags them) those
    /// answering its own request. A page at offset 0 replaces the list, the
    /// next page appends, any other page is ignored. If the next page comes
    /// from a different download of the list than the pages held, nothing
    /// is appended: the list is emptied and
    /// [`take_restart`](Self::take_restart) asks for it from the start.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetBlocklistEntries {
                subscription_id,
                entries,
                offset,
                total,
                request_id,
                last_updated_iso8601,
            } => {
                if request_id
                    .as_deref()
                    .is_some_and(|id| id != client_request_id())
                    || self.wanted.as_deref() != Some(subscription_id.as_str())
                {
                    return false;
                }
                let hosts = entries.iter().map(|e| e.host.clone());
                if *offset == 0 {
                    self.subscription_id = subscription_id.clone();
                    self.hosts = hosts.collect();
                    self.download = last_updated_iso8601.clone();
                    self.restart = None;
                } else if *subscription_id == self.subscription_id
                    && *offset == self.hosts.len() as u64
                {
                    if *last_updated_iso8601 != self.download {
                        self.subscription_id.clear();
                        self.hosts.clear();
                        self.download = None;
                        self.total = 0;
                        self.restart = Some(subscription_id.clone());
                        return true;
                    } else {
                        self.hosts.extend(hosts);
                    }
                } else {
                    return false;
                }
                self.total = *total;
                true
            }
            _ => false,
        }
    }

    /// The list to request again from offset 0, once, after
    /// [`apply`](Self::apply) found its contents changed mid-way.
    pub fn take_restart(&mut self) -> Option<String> {
        self.restart.take()
    }

    /// Clear the detail list (e.g. when the selection is cleared).
    pub fn clear(&mut self) -> bool {
        self.wanted = None;
        self.download = None;
        self.restart = None;
        if self.hosts.is_empty() && self.subscription_id.is_empty() {
            return false;
        }
        self.subscription_id.clear();
        self.hosts.clear();
        self.total = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::BlocklistEntry;

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
            request_id: None,
            last_updated_iso8601: None,
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
            request_id: None,
            last_updated_iso8601: None,
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
            request_id: None,
            last_updated_iso8601: None,
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
        page_of(id, hosts, offset, total, None, None)
    }

    fn page_of(
        id: &str,
        hosts: &[&str],
        offset: u64,
        total: u64,
        request_id: Option<&str>,
        download: Option<&str>,
    ) -> ServerMessage {
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
            request_id: request_id.map(str::to_string),
            last_updated_iso8601: download.map(str::to_string),
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

    /// Issue #67: pages are broadcast to every GUI, each tagged with the
    /// request it answers. Only this GUI's are kept; a page with no tag comes
    /// from an older bridge and is judged by its list alone.
    #[test]
    fn pages_answering_another_guis_request_are_ignored() {
        let mut e = EntriesStore::new();
        e.expect("a");
        let ours = client_request_id();
        assert_ne!(ours, "another-gui");
        assert!(!e.apply(&page_of("a", &["x.x"], 0, 1, Some("another-gui"), None)));
        assert!(e.is_empty());
        assert!(e.apply(&page_of("a", &["1.x"], 0, 1, Some(ours), None)));
        assert!(e.apply(&page_of("a", &["2.x"], 0, 1, None, None)));
        assert_eq!(e.hosts(), &["2.x".to_string()]);
    }

    #[test]
    fn this_guis_request_id_is_stable_and_short() {
        let id = client_request_id();
        assert_eq!(id, client_request_id());
        assert!(!id.is_empty() && id.len() <= snitchwatch_bridge::ws_messages::MAX_REQUEST_ID_LEN);
    }

    /// Issue #67: pages fetched across a refresh must not be mixed. A later
    /// page from another download drops what was loaded and asks to start
    /// over; the first page of the new download then fills the list.
    #[test]
    fn a_later_page_from_a_newer_download_starts_the_list_over() {
        let mut e = EntriesStore::new();
        e.expect("a");
        assert!(e.apply(&page_of("a", &["1.x", "2.x"], 0, 4, None, Some("t1"))));
        assert_eq!(e.take_restart(), None);

        assert!(e.apply(&page_of("a", &["n3.x", "n4.x"], 2, 4, None, Some("t2"))));
        assert!(e.is_empty(), "no old and new hosts together");
        assert_eq!(e.subscription_id(), "");
        assert!(!e.has_more());
        assert_eq!(e.take_restart().as_deref(), Some("a"));
        assert_eq!(e.take_restart(), None, "asked for once");

        assert!(e.apply(&page_of("a", &["n1.x", "n2.x"], 0, 4, None, Some("t2"))));
        assert!(e.apply(&page_of("a", &["n3.x", "n4.x"], 2, 4, None, Some("t2"))));
        assert_eq!(e.len(), 4);
        assert_eq!(e.host(3), Some("n4.x"));
        assert_eq!(e.take_restart(), None);
    }

    #[test]
    fn pages_of_one_download_append_and_a_new_first_page_just_replaces() {
        let mut e = EntriesStore::new();
        e.expect("a");
        assert!(e.apply(&page_of("a", &["1.x"], 0, 2, None, Some("t1"))));
        assert!(e.apply(&page_of("a", &["2.x"], 1, 2, None, Some("t1"))));
        assert_eq!(e.len(), 2);
        // The inspector reopened after a refresh: no restart needed.
        assert!(e.apply(&page_of("a", &["n1.x"], 0, 1, None, Some("t2"))));
        assert_eq!(e.hosts(), &["n1.x".to_string()]);
        assert_eq!(e.take_restart(), None);
    }

    /// A bridge that sends no download time (older) is never "different".
    #[test]
    fn pages_without_a_download_time_never_restart() {
        let mut e = EntriesStore::new();
        e.expect("a");
        assert!(e.apply(&page("a", &["1.x"], 0, 2)));
        assert!(e.apply(&page("a", &["2.x"], 1, 2)));
        assert_eq!(e.len(), 2);
        assert_eq!(e.take_restart(), None);
    }

    /// The restart belongs to the list that asked: choosing another list or
    /// clearing the inspector cancels it.
    #[test]
    fn a_pending_restart_is_cancelled_by_choosing_another_list_or_clearing() {
        let mut e = EntriesStore::new();
        e.expect("a");
        e.apply(&page_of("a", &["1.x"], 0, 2, None, Some("t1")));
        e.apply(&page_of("a", &["2.x"], 1, 2, None, Some("t2")));
        e.expect("b");
        assert_eq!(e.take_restart(), None);

        e.expect("a");
        e.apply(&page_of("a", &["1.x"], 0, 2, None, Some("t1")));
        e.apply(&page_of("a", &["2.x"], 1, 2, None, Some("t2")));
        e.clear();
        assert_eq!(e.take_restart(), None);
    }
}
