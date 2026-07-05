//! Pure, Qt-free filtering + selection policy for the connections list
//! (Task 8 remainder).
//!
//! `web/js/connections.js` computes its visible-row set and its
//! auto-select-on-new-pending behaviour in JS; the plan requires that logic to
//! live in Rust ("do not reimplement filtering in QML"), unit-tested without a
//! Qt runtime. This module holds two pure pieces:
//!
//!   * [`ConnectionFilter`] — the case-insensitive substring + pending-only
//!     predicate the search box drives.
//!   * [`auto_select_target`] — the rule deciding whether a freshly arrived
//!     pending row should be auto-selected, honouring the design spec's "won't
//!     steal focus from a row the user is investigating" constraint.
//!
//! [`super::row_store::RowStore`] owns a `ConnectionFilter` and projects a
//! `visible` index list from it; the cxx-qt wrapper reads that projection.

use snitchwatch_bridge::ws_messages::ConnectionRow;

/// The connections search/filter predicate.
///
/// `query` is matched case-insensitively as a substring across the fields a
/// user would search by (process, host, ip, port, protocol, verdict). Empty
/// query matches everything. `pending_only` additionally restricts to rows
/// still awaiting a decision (`action == None`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionFilter {
    query: String,
    pending_only: bool,
}

impl ConnectionFilter {
    pub fn new(query: impl Into<String>, pending_only: bool) -> Self {
        Self {
            query: query.into(),
            pending_only,
        }
    }

    /// True when the filter would narrow the list (a non-blank query or the
    /// pending-only toggle). When inactive, callers can skip projection work
    /// entirely and treat the visible list as identity over the full store.
    pub fn is_active(&self) -> bool {
        self.pending_only || !self.query.trim().is_empty()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn pending_only(&self) -> bool {
        self.pending_only
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
    }

    pub fn set_pending_only(&mut self, pending_only: bool) {
        self.pending_only = pending_only;
    }

    /// Does `row` pass this filter?
    pub fn matches(&self, row: &ConnectionRow) -> bool {
        if self.pending_only && row.action.is_some() {
            return false;
        }
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let verdict = row.action.as_deref().unwrap_or("pending");
        let haystack = [
            row.process.as_str(),
            row.dst_host.as_str(),
            row.dst_ip.as_str(),
            row.protocol.as_str(),
            verdict,
        ];
        if haystack.iter().any(|f| f.to_lowercase().contains(&needle)) {
            return true;
        }
        // Port is numeric; match it as text too so "443" filters by port.
        row.dst_port.to_string().contains(&needle)
    }
}

/// Decide whether a newly arrived pending row should become the selection.
///
/// Returns `Some(id)` when the UI should move its selection to `id`, or `None`
/// to leave the current selection untouched.
///
/// Policy (from the design spec's "auto-select the new pending row, but won't
/// steal focus from a row the user is investigating"):
///   * If the user is currently sitting on a *pending* row, they are actively
///     making a decision — never move the selection out from under them.
///   * Otherwise (no selection, or the selected row is already decided),
///     auto-select the first newly arrived pending row so the decision surfaces.
///
/// `new_pending_ids` is the ordered list of ids that arrived pending in the
/// batch just applied; the first is chosen because inserts append in arrival
/// order.
pub fn auto_select_target(
    current: Option<&str>,
    current_is_pending: bool,
    new_pending_ids: &[&str],
) -> Option<String> {
    if current.is_some() && current_is_pending {
        return None;
    }
    new_pending_ids.first().map(|id| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, process: &str, host: &str, port: u16, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: process.to_string(),
            process_path: None,
            dst_host: host.to_string(),
            dst_ip: "10.0.0.1".to_string(),
            dst_port: port,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: action.map(|s| s.to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }
    }

    #[test]
    fn empty_filter_is_inactive_and_matches_all() {
        let f = ConnectionFilter::default();
        assert!(!f.is_active());
        assert!(f.matches(&row("a", "firefox", "github.com", 443, None)));
        assert!(f.matches(&row("b", "slack", "slack.com", 443, Some("allow"))));
    }

    #[test]
    fn whitespace_only_query_is_inactive() {
        let f = ConnectionFilter::new("   ", false);
        assert!(!f.is_active());
    }

    #[test]
    fn query_matches_process_host_and_is_case_insensitive() {
        let f = ConnectionFilter::new("GITHUB", false);
        assert!(f.is_active());
        assert!(f.matches(&row("a", "firefox", "github.com", 443, None)));
        assert!(!f.matches(&row("b", "slack", "slack.com", 443, None)));

        let f = ConnectionFilter::new("fire", false);
        assert!(f.matches(&row("a", "firefox", "github.com", 443, None)));
    }

    #[test]
    fn query_matches_port_as_text() {
        let f = ConnectionFilter::new("8080", false);
        assert!(f.matches(&row("a", "proc", "h.example", 8080, None)));
        assert!(!f.matches(&row("b", "proc", "h.example", 443, None)));
    }

    #[test]
    fn query_matches_verdict_token_including_pending() {
        assert!(ConnectionFilter::new("pending", false).matches(&row("a", "p", "h", 1, None)));
        assert!(ConnectionFilter::new("deny", false).matches(&row("a", "p", "h", 1, Some("deny"))));
        assert!(!ConnectionFilter::new("deny", false).matches(&row(
            "a",
            "p",
            "h",
            1,
            Some("allow")
        )));
    }

    #[test]
    fn pending_only_excludes_decided_rows() {
        let f = ConnectionFilter::new("", true);
        assert!(f.is_active());
        assert!(f.matches(&row("a", "p", "h", 1, None)));
        assert!(!f.matches(&row("b", "p", "h", 1, Some("allow"))));
    }

    #[test]
    fn pending_only_and_query_combine() {
        let f = ConnectionFilter::new("firefox", true);
        // pending + matches query
        assert!(f.matches(&row("a", "firefox", "h", 1, None)));
        // decided, excluded by pending_only even though query matches
        assert!(!f.matches(&row("b", "firefox", "h", 1, Some("allow"))));
        // pending but query misses
        assert!(!f.matches(&row("c", "chrome", "h", 1, None)));
    }

    #[test]
    fn auto_select_picks_first_new_pending_when_nothing_selected() {
        assert_eq!(
            auto_select_target(None, false, &["p1", "p2"]),
            Some("p1".to_string())
        );
    }

    #[test]
    fn auto_select_moves_off_a_decided_selection() {
        // Current selection is a decided row -> ok to surface the new pending one.
        assert_eq!(
            auto_select_target(Some("old"), false, &["p1"]),
            Some("p1".to_string())
        );
    }

    #[test]
    fn auto_select_does_not_steal_focus_from_a_pending_row() {
        // User is investigating a pending row -> leave selection alone.
        assert_eq!(auto_select_target(Some("busy"), true, &["p1"]), None);
    }

    #[test]
    fn auto_select_none_when_no_new_pending() {
        assert_eq!(auto_select_target(None, false, &[]), None);
        assert_eq!(auto_select_target(Some("x"), false, &[]), None);
    }
}
