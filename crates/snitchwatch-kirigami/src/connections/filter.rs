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

use super::row_store::Verdict;
use crate::geo::resolver::{Bucket, SharedResolver};

/// One parsed search term. Terms are separated by whitespace and AND-ed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    /// `app:` — substring of the process name or path, case-insensitive.
    App(String),
    /// `host:` — substring of the destination host or IP, case-insensitive.
    Host(String),
    /// `port:` — exact destination port.
    Port(u16),
    /// `port:` with a value that is not a port number (`port:abc`, `port:99999`).
    /// Matches nothing, so a typo never silently shows every row.
    BadPort(String),
    /// `country:` — ISO-A2 code, from the geo resolver.
    Country(String),
    /// Anything else, including unknown prefixes (`ip:1.2.3.4`): a
    /// case-insensitive substring over process, host, ip, protocol, verdict
    /// and port text.
    Free(String),
}

/// Split `query` on whitespace, keeping a double-quoted phrase together with
/// its quotes stripped. An unterminated quote takes the rest of the string.
/// The flag is true when the token started with a quote: a phrase is always
/// free text, even if it contains a colon.
fn split_tokens(query: &str) -> Vec<(String, bool)> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started_quoted = false;
    let mut in_token = false;
    let mut in_quote = false;
    for ch in query.chars() {
        match ch {
            '"' => {
                if !in_token {
                    in_token = true;
                    started_quoted = true;
                }
                in_quote = !in_quote;
            }
            c if c.is_whitespace() && !in_quote => {
                if in_token {
                    tokens.push((std::mem::take(&mut current), started_quoted));
                    in_token = false;
                    started_quoted = false;
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        tokens.push((current, started_quoted));
    }
    tokens
}

/// Split `query` into terms. Blank input, an empty phrase (`""`) and a known
/// prefix with an empty value (`host:`) yield no term; an unknown prefix is
/// free text.
fn parse_terms(query: &str) -> Vec<Term> {
    split_tokens(query)
        .into_iter()
        .filter_map(|(token, phrase)| {
            let lower = token.to_lowercase();
            if phrase {
                return (!lower.is_empty()).then_some(Term::Free(lower));
            }
            let Some((prefix, value)) = lower.split_once(':') else {
                return Some(Term::Free(lower));
            };
            let known = matches!(prefix, "app" | "host" | "port" | "country");
            if !known {
                return Some(Term::Free(lower));
            }
            if value.is_empty() {
                return None;
            }
            Some(match prefix {
                "app" => Term::App(value.to_string()),
                "host" => Term::Host(value.to_string()),
                "port" => value
                    .parse::<u16>()
                    .map(Term::Port)
                    .unwrap_or_else(|_| Term::BadPort(value.to_string())),
                _ => Term::Country(value.to_uppercase()),
            })
        })
        .collect()
}

/// The connections search/filter predicate.
///
/// `query` is a whitespace-separated list of terms, all of which must match
/// (see [`Term`]; the prefixes are `app:`, `host:`, `port:`, `country:`).
/// Empty query matches everything. `pending_only` restricts to rows still
/// awaiting a decision, `denied_only` to rows whose verdict is denied, and
/// `since_ms` ("From now") to rows that started at or after that instant.
#[derive(Debug, Clone, Default)]
pub struct ConnectionFilter {
    query: String,
    terms: Vec<Term>,
    pending_only: bool,
    denied_only: bool,
    /// Session-only: set when "From now" is turned on, never persisted.
    since_ms: Option<i64>,
    /// Resolves a row's country for `country:`. Not part of the filter's
    /// identity (see the `PartialEq` impl): it is fixed for the model's life.
    geo: Option<SharedResolver>,
}

impl PartialEq for ConnectionFilter {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query
            && self.pending_only == other.pending_only
            && self.denied_only == other.denied_only
            && self.since_ms == other.since_ms
    }
}

impl Eq for ConnectionFilter {}

impl ConnectionFilter {
    pub fn new(query: impl Into<String>, pending_only: bool) -> Self {
        let query = query.into();
        Self {
            terms: parse_terms(&query),
            query,
            pending_only,
            ..Self::default()
        }
    }

    /// Attach the country resolver `country:` terms consult.
    pub fn with_geo(mut self, geo: SharedResolver) -> Self {
        self.geo = Some(geo);
        self
    }

    /// True when the filter would narrow the list. When inactive, callers can
    /// skip projection work entirely and treat the visible list as identity
    /// over the full store.
    pub fn is_active(&self) -> bool {
        self.pending_only || self.denied_only || self.since_ms.is_some() || !self.terms.is_empty()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn pending_only(&self) -> bool {
        self.pending_only
    }

    pub fn denied_only(&self) -> bool {
        self.denied_only
    }

    pub fn since_ms(&self) -> Option<i64> {
        self.since_ms
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.terms = parse_terms(&self.query);
    }

    pub fn set_pending_only(&mut self, pending_only: bool) {
        self.pending_only = pending_only;
    }

    pub fn set_denied_only(&mut self, denied_only: bool) {
        self.denied_only = denied_only;
    }

    /// `Some(ms)` hides rows that started before `ms`; `None` clears it.
    pub fn set_since_ms(&mut self, since_ms: Option<i64>) {
        self.since_ms = since_ms;
    }

    /// Plain-text description of the active filter for the UI; empty when
    /// inactive. Says so when a term cannot match anything, rather than
    /// showing an unexplained empty list.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.terms.is_empty() {
            parts.push(format!("search \"{}\"", self.query.trim()));
        }
        if self.pending_only {
            parts.push("pending only".to_string());
        }
        if self.denied_only {
            parts.push("denied only".to_string());
        }
        if self.since_ms.is_some() {
            // See `matches`: the start time is compared, not activity.
            parts.push(
                "from now (hides rows that started earlier, even if still active)".to_string(),
            );
        }
        let mut text = parts.join(", ");
        if self.pending_only && self.denied_only {
            if !text.is_empty() {
                text.push_str(". ");
            }
            text.push_str("Pending only and denied only cannot both match, so no rows match.");
        }
        for term in &self.terms {
            let note = match term {
                Term::BadPort(v) => format!("port:{v} is not a port number, so no rows match."),
                Term::Country(_) if !self.geo.as_ref().is_some_and(|g| g.has_database()) => {
                    "country: matches nothing because no GeoIP database is installed.".to_string()
                }
                _ => continue,
            };
            if !text.is_empty() {
                text.push_str(". ");
            }
            text.push_str(&note);
        }
        text
    }

    /// Does `row` pass this filter?
    pub fn matches(&self, row: &ConnectionRow) -> bool {
        if self.pending_only && !super::outcome::is_pending(row) {
            return false;
        }
        if self.denied_only && Verdict::of(row) != Verdict::Denied {
            return false;
        }
        // "From now" compares the row's start time, not its activity: a
        // connection opened before the switch stays hidden even while it is
        // still active. The summary says so.
        if self.since_ms.is_some_and(|since| row.started_at_ms < since) {
            return false;
        }
        self.terms.iter().all(|term| self.term_matches(term, row))
    }

    fn term_matches(&self, term: &Term, row: &ConnectionRow) -> bool {
        match term {
            Term::App(v) => {
                row.process.to_lowercase().contains(v)
                    || row
                        .process_path
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(v))
            }
            Term::Host(v) => {
                row.dst_host.to_lowercase().contains(v) || row.dst_ip.to_lowercase().contains(v)
            }
            Term::Port(p) => row.dst_port == *p,
            Term::BadPort(_) => false,
            Term::Country(code) => self.geo.as_ref().is_some_and(
                |geo| matches!(geo.resolve(&row.dst_ip).bucket, Bucket::Country(c) if c == *code),
            ),
            Term::Free(needle) => {
                let verdict = row.action.as_deref().unwrap_or("pending");
                let haystack = [
                    row.process.as_str(),
                    row.dst_host.as_str(),
                    row.dst_ip.as_str(),
                    row.protocol.as_str(),
                    verdict,
                ];
                // Port is numeric; match it as text too so "443" filters by port.
                haystack.iter().any(|f| f.to_lowercase().contains(needle))
                    || row.dst_port.to_string().contains(needle)
            }
        }
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
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
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

    use crate::geo::resolver::fakes::FakeLookup;
    use std::sync::Arc;

    fn ip_row(
        id: &str,
        process: &str,
        host: &str,
        ip: &str,
        port: u16,
        started: i64,
    ) -> ConnectionRow {
        let mut r = row(id, process, host, port, Some("allow"));
        r.dst_ip = ip.to_string();
        r.started_at_ms = started;
        r
    }

    fn with_geo(query: &str) -> ConnectionFilter {
        let lookup = FakeLookup::default().with("1.1.1.1", "DE", "Germany").with(
            "2.2.2.2",
            "US",
            "United States",
        );
        ConnectionFilter::new(query, false).with_geo(SharedResolver::new(Some(Arc::new(lookup))))
    }

    #[test]
    fn app_token_is_a_case_insensitive_substring_of_process_or_path() {
        let f = ConnectionFilter::new("app:FIRE", false);
        assert!(f.matches(&row("a", "firefox", "slack.com", 443, None)));
        assert!(!f.matches(&row("b", "slack", "firefox.com", 443, None)));
        let mut r = row("c", "bin", "h", 1, None);
        r.process_path = Some("/usr/lib/Firefox/bin".to_string());
        assert!(f.matches(&r));
    }

    #[test]
    fn host_token_is_a_case_insensitive_substring_of_host_or_ip() {
        let f = ConnectionFilter::new("host:GitHub", false);
        assert!(f.matches(&row("a", "p", "api.github.com", 443, None)));
        assert!(!f.matches(&row("b", "github", "slack.com", 443, None)));
        assert!(ConnectionFilter::new("host:10.0.", false).matches(&row("c", "p", "", 1, None)));
    }

    #[test]
    fn port_token_is_exact() {
        let f = ConnectionFilter::new("port:443", false);
        assert!(f.matches(&row("a", "p", "h", 443, None)));
        assert!(!f.matches(&row("b", "p", "h", 4430, None)));
        assert!(!f.matches(&row("c", "p", "h", 44, None)));
    }

    #[test]
    fn unparseable_port_matches_nothing_and_the_summary_says_so() {
        for q in ["port:abc", "port:99999", "port:-1"] {
            let f = ConnectionFilter::new(q, false);
            assert!(f.is_active(), "{q}");
            assert!(!f.matches(&row("a", "p", "h", 443, None)), "{q}");
            assert!(f.summary().contains("not a port number"), "{q}");
        }
    }

    #[test]
    fn tokens_are_anded_in_any_order() {
        let f = ConnectionFilter::new("host:github port:443", false);
        assert!(f.matches(&row("a", "p", "github.com", 443, None)));
        assert!(!f.matches(&row("b", "p", "github.com", 80, None)));
        assert!(!f.matches(&row("c", "p", "slack.com", 443, None)));
        let f = ConnectionFilter::new("port:443 host:github", false);
        assert!(f.matches(&row("a", "p", "github.com", 443, None)));
        let f = ConnectionFilter::new("app:fire host:github deny", false);
        assert!(f.matches(&row("a", "firefox", "github.com", 1, Some("deny"))));
        assert!(!f.matches(&row("b", "firefox", "github.com", 1, Some("allow"))));
    }

    #[test]
    fn prefixes_are_case_insensitive() {
        let f = ConnectionFilter::new("HOST:github PORT:443", false);
        assert!(f.matches(&row("a", "p", "github.com", 443, None)));
    }

    #[test]
    fn blank_whitespace_and_empty_value_tokens_are_ignored() {
        for q in [
            "",
            "   ",
            "\t \n",
            "host:",
            "app:",
            "port:",
            "country:",
            "host: port:  ",
        ] {
            let f = ConnectionFilter::new(q, false);
            assert!(!f.is_active(), "{q:?} should be inactive");
            assert!(f.matches(&row("a", "p", "h", 1, None)), "{q:?}");
        }
        // An empty token beside a real one leaves the real one in force.
        let f = ConnectionFilter::new("  host:  app:fire ", false);
        assert!(f.matches(&row("a", "firefox", "h", 1, None)));
        assert!(!f.matches(&row("b", "slack", "h", 1, None)));
    }

    #[test]
    fn unknown_prefixes_are_free_text() {
        // "ip:" is not a token kind, so the whole token is a free-text needle
        // that appears in no field.
        let f = ConnectionFilter::new("ip:10.0.0.1", false);
        assert!(f.is_active());
        assert!(!f.matches(&row("a", "p", "h", 1, None)));
        let mut r = row("b", "weird:name", "h", 1, None);
        r.dst_ip = "x".to_string();
        assert!(ConnectionFilter::new("weird:name", false).matches(&r));
        assert!(ConnectionFilter::new("::1", false).is_active());
    }

    #[test]
    fn free_text_still_searches_verdict_and_port_text() {
        assert!(ConnectionFilter::new("pending", false).matches(&row("a", "p", "h", 1, None)));
        assert!(ConnectionFilter::new("844", false).matches(&row("a", "p", "h", 8443, None)));
    }

    #[test]
    fn country_matches_the_resolved_country_case_insensitively() {
        let f = with_geo("country:de");
        assert!(f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 0)));
        assert!(!f.matches(&ip_row("b", "p", "h", "2.2.2.2", 1, 0)));
        // Unresolvable and local addresses never match a country.
        assert!(!f.matches(&ip_row("c", "p", "h", "9.9.9.9", 1, 0)));
        assert!(!f.matches(&ip_row("d", "p", "h", "10.0.0.1", 1, 0)));
        assert!(with_geo("COUNTRY:US").matches(&ip_row("b", "p", "h", "2.2.2.2", 1, 0)));
        assert_eq!(f.summary(), "search \"country:de\"");
    }

    #[test]
    fn country_combines_with_other_tokens() {
        let f = with_geo("country:DE host:example");
        assert!(f.matches(&ip_row("a", "p", "example.org", "1.1.1.1", 1, 0)));
        assert!(!f.matches(&ip_row("b", "p", "other.org", "1.1.1.1", 1, 0)));
    }

    #[test]
    fn country_matches_nothing_without_a_database_and_says_so() {
        for geo in [None, Some(SharedResolver::new(None))] {
            let mut f = ConnectionFilter::new("country:DE", false);
            if let Some(g) = geo {
                f = f.with_geo(g);
            }
            assert!(f.is_active());
            assert!(!f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 0)));
            assert!(f.summary().contains("no GeoIP database is installed"));
        }
    }

    #[test]
    fn since_boundary_keeps_rows_at_or_after_the_start() {
        let mut f = ConnectionFilter::default();
        f.set_since_ms(Some(1_000));
        assert!(f.is_active());
        assert!(!f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 999)));
        assert!(f.matches(&ip_row("b", "p", "h", "1.1.1.1", 1, 1_000)));
        assert!(f.matches(&ip_row("c", "p", "h", "1.1.1.1", 1, 1_001)));
        assert_eq!(f.since_ms(), Some(1_000));
        f.set_since_ms(None);
        assert!(!f.is_active());
        assert!(f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 999)));
    }

    #[test]
    fn denied_only_keeps_denied_rows_including_default_denials() {
        let mut f = ConnectionFilter::default();
        f.set_denied_only(true);
        assert!(f.is_active());
        assert!(f.matches(&row("a", "p", "h", 1, Some("deny"))));
        assert!(!f.matches(&row("b", "p", "h", 1, Some("allow"))));
        assert!(!f.matches(&row("c", "p", "h", 1, None)));
        let mut d = row("d", "p", "h", 1, Some("deny"));
        d.decided_by_default = true;
        assert!(f.matches(&d));
    }

    #[test]
    fn denied_only_and_pending_only_together_match_nothing() {
        // A row cannot be both undecided and denied.
        let mut f = ConnectionFilter::default();
        f.set_denied_only(true);
        f.set_pending_only(true);
        for a in [None, Some("deny"), Some("allow")] {
            assert!(!f.matches(&row("a", "p", "h", 1, a)), "{a:?}");
        }
        f.set_pending_only(false);
        assert!(f.matches(&row("a", "p", "h", 1, Some("deny"))));
    }

    #[test]
    fn every_option_combines_and_clearing_each_restores_the_list() {
        let mut f = with_geo("country:DE app:fire");
        f.set_denied_only(true);
        f.set_since_ms(Some(100));
        let mut hit = ip_row("a", "firefox", "h", "1.1.1.1", 1, 100);
        hit.action = Some("deny".to_string());
        assert!(f.matches(&hit));
        let mut old = hit.clone();
        old.started_at_ms = 99;
        assert!(!f.matches(&old));
        f.set_since_ms(None);
        f.set_denied_only(false);
        f.set_query("");
        assert!(!f.is_active());
    }

    #[test]
    fn equality_ignores_the_resolver_but_not_the_options() {
        assert_eq!(with_geo("host:a"), ConnectionFilter::new("host:a", false));
        let mut f = ConnectionFilter::default();
        f.set_denied_only(true);
        assert_ne!(f, ConnectionFilter::default());
        let mut g = ConnectionFilter::default();
        g.set_since_ms(Some(1));
        assert_ne!(g, ConnectionFilter::default());
    }

    #[test]
    fn summary_is_empty_when_inactive_and_lists_active_options() {
        assert_eq!(ConnectionFilter::default().summary(), "");
        let mut f = ConnectionFilter::new("host:a", true);
        f.set_denied_only(true);
        f.set_since_ms(Some(5));
        assert_eq!(
            f.summary(),
            "search \"host:a\", pending only, denied only, \
             from now (hides rows that started earlier, even if still active). \
             Pending only and denied only cannot both match, so no rows match."
        );
        let mut both = ConnectionFilter::default();
        both.set_pending_only(true);
        both.set_denied_only(true);
        assert!(both.summary().contains("cannot both match"));
        let mut one = ConnectionFilter::default();
        one.set_pending_only(true);
        assert!(!one.summary().contains("cannot both match"));
    }

    #[test]
    fn repeated_tokens_are_anded() {
        let f = ConnectionFilter::new("host:a host:b", false);
        assert!(f.matches(&row("x", "p", "a.b.com", 1, None)));
        assert!(!f.matches(&row("y", "p", "a.com", 1, None)));
        assert!(!f.matches(&row("z", "p", "b.com", 1, None)));
    }

    #[test]
    fn quoted_phrases_are_one_free_text_term() {
        assert_eq!(
            parse_terms("\"foo bar\" baz"),
            vec![Term::Free("foo bar".into()), Term::Free("baz".into())]
        );
        // Unterminated: the rest of the string is the phrase.
        assert_eq!(
            parse_terms("a \"foo bar baz"),
            vec![Term::Free("a".into()), Term::Free("foo bar baz".into())]
        );
        // A phrase is free text even with a colon in it; empty phrases vanish.
        assert_eq!(
            parse_terms("\"host:x\" \"\""),
            vec![Term::Free("host:x".into())]
        );
        // A quoted value after a prefix keeps its spaces.
        assert_eq!(
            parse_terms("app:\"my app\""),
            vec![Term::App("my app".into())]
        );
        let f = ConnectionFilter::new("\"foo bar\"", false);
        assert!(f.matches(&row("a", "foo bar", "h", 1, None)));
        assert!(!f.matches(&row("b", "foo", "bar", 1, None)));
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
    #[test]
    fn geographic_scope_preserves_query_and_matches_canonical_ip_exactly() {
        let mut f = ConnectionFilter::new("app:fire", false);
        f.set_geo_scope(GeoScope::parse("ip", "ip:2001:4860:4860::8888"));
        let matching = ip_row("a", "firefox", "dns", "2001:4860:4860:0:0:0:0:8888", 443, 0);
        assert!(f.matches(&matching));
        assert!(!f.matches(&ip_row("b", "firefox", "dns", "2001:4860:4860::8889", 443, 0)));
        assert!(!f.matches(&ip_row("c", "other", "dns", "2001:4860:4860::8888", 443, 0)));
        f.set_geo_scope(GeoScope::parse("ip", "ip:1.1.1.1"));
        assert!(!f.matches(&ip_row("d", "firefox", "dns", "11.1.1.1", 443, 0)));
        assert_eq!(f.query(), "app:fire");
        f.set_geo_scope(None);
        assert!(f.matches(&matching));
        assert_eq!(f.query(), "app:fire");
    }

    #[test]
    fn geographic_scope_country_and_unavailable_city_use_shared_bucket_identity() {
        let mut f = with_geo("app:fire");
        f.set_geo_scope(GeoScope::parse("country", "country:DE"));
        assert!(f.matches(&ip_row("a", "firefox", "dns", "1.1.1.1", 443, 0)));
        assert!(!f.matches(&ip_row("b", "firefox", "dns", "2.2.2.2", 443, 0)));
        f.set_geo_scope(GeoScope::parse("city", "country:DE:unknown-city"));
        assert!(f.matches(&ip_row("a", "firefox", "dns", "1.1.1.1", 443, 0)));
        f.set_geo_scope(GeoScope::parse("country", "local"));
        assert!(f.matches(&ip_row("c", "firefox", "router", "192.168.1.1", 443, 0)));
        f.set_geo_scope(GeoScope::parse("country", "unknown"));
        assert!(f.matches(&ip_row("d", "firefox", "dns", "9.9.9.9", 443, 0)));
        assert!(!f.matches(&ip_row("c", "firefox", "router", "192.168.1.1", 443, 0)));
    }

    #[test]
    fn geographic_scope_city_uses_shared_composite_key_including_subdivision() {
        use crate::geo::resolver::{CountryInfo,CountryLookup,GeoPlace,GeoCoordinates};
        struct Cities;
        impl CountryLookup for Cities {
            fn lookup(&self,_ip:std::net::IpAddr)->Option<CountryInfo> { None }
            fn lookup_place(&self,ip:std::net::IpAddr)->Option<GeoPlace> {
                Some(GeoPlace {country:CountryInfo {code:"US".into(),name:"United States".into()},
                    subdivisions:vec![if ip.to_string()=="1.1.1.1" {"North"} else {"South"}.into()],
                    city:Some("Same city".into()),coordinates:GeoCoordinates::new(Some(1.0),Some(2.0)),
                    provider:"fixture".into(),precision:None})
            }
        }
        let geo=SharedResolver::new(Some(Arc::new(Cities)));
        let key=geo.resolve("1.1.1.1").place.unwrap().city_key();
        let mut f=ConnectionFilter::default().with_geo(geo);
        f.set_geo_scope(GeoScope::parse("city",&key));
        assert!(f.is_active());
        assert!(f.matches(&ip_row("a","firefox","h","1.1.1.1",443,0)));
        assert!(!f.matches(&ip_row("b","firefox","h","2.2.2.2",443,0)));
        let old=f.clone();
        f.set_geo_scope(GeoScope::parse("city","city:removed"));
        assert_ne!(f,old,"scope participates in projection identity");
        assert!(!f.matches(&ip_row("a","firefox","h","1.1.1.1",443,0)),"vanished city stays empty");
    }

    #[test]
    fn geographic_scope_rejects_invalid_inputs() {
        for (kind,key) in [("ip","ip:1.1.1"),("ip","1.1.1.1"),("country","country:DEU"),
            ("country","country:de"),("city",""),("city","country:DE"),("wat","country:DE")] {
            assert!(GeoScope::parse(kind,key).is_none(),"{kind}: {key}");
        }
    }

    #[test]
    fn prepared_geographic_scope_never_decodes_after_shared_cache_overflow() {
        use crate::geo::resolver::{CountryInfo,CountryLookup};
        use std::sync::atomic::{AtomicUsize,Ordering};
        struct Counting(Arc<AtomicUsize>);
        impl CountryLookup for Counting {
            fn lookup(&self,_ip:std::net::IpAddr)->Option<CountryInfo>{
                self.0.fetch_add(1,Ordering::SeqCst);
                Some(CountryInfo {code:"DE".into(),name:"Germany".into()})
            }
        }
        let calls=Arc::new(AtomicUsize::new(0));
        let geo=SharedResolver::new(Some(Arc::new(Counting(calls.clone()))));
        let r=ip_row("prepared","browser","dns","1.1.1.1",443,0);
        let mut facts=std::collections::HashMap::new();facts.insert(r.id.clone(),geo.resolve(&r.dst_ip));
        for i in 0..65_538u32 {geo.resolve(&std::net::Ipv4Addr::from(0x0b00_0000+i).to_string());}
        let before=calls.load(Ordering::SeqCst);
        let mut f=ConnectionFilter::default().with_geo(geo).prepared_only();
        f.merge_prepared_geo(facts);
        for scope in [("country","country:DE"),("city","country:DE:unknown-city"),("ip","ip:1.1.1.1")] {
            f.set_geo_scope(GeoScope::parse(scope.0,scope.1));assert!(f.matches(&r));
        }
        let mut untracked=r.clone();untracked.id="outside-geo-capacity".into();
        assert!(!f.matches(&untracked),"missing prepared facts must not masquerade as Unknown or broaden scope");
        f.set_query("country:DE");f.set_geo_scope(None);assert!(f.matches(&r));
        assert_eq!(calls.load(Ordering::SeqCst),before,"scope evaluation decoded MMDB on Qt-equivalent path");
    }

}
