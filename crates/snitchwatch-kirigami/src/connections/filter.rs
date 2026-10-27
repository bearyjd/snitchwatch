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

pub use super::geo_scope::GeoScope;
use snitchwatch_bridge::ws_messages::ConnectionRow;
use std::{collections::HashMap, sync::Arc};

use super::attention::AttentionFacts;
use super::geo_scope::GeoIdentity;
use super::row_store::Verdict;
use crate::geo::resolver::{Resolved, SharedResolver};
use std::borrow::Cow;

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
    geo_scope: Option<GeoScope>,
    // Some means production prepared-only mode: absent records never decode
    // the MMDB or masquerade as unknown locations during Qt-side filtering.
    prepared_geo: Option<Arc<HashMap<String, GeoIdentity>>>,
    query: String,
    terms: Vec<Term>,
    pending_only: bool,
    denied_only: bool,
    /// Session-only: set when "From now" is turned on, never persisted.
    since_ms: Option<i64>,
    /// Exception-first default (0c): collapse handled rows
    /// (`super::attention`). ON in the model's filter by default.
    needs_attention: bool,
    /// What "handled" reads. Not part of the filter's identity, like `geo`:
    /// it changes with the rules list and the seen set, through
    /// [`Self::set_attention_facts`]. `None` handles nothing (fail open).
    attention: Option<Arc<AttentionFacts>>,
    /// Resolves a row's country for `country:`. Not part of the filter's
    /// identity (see the `PartialEq` impl): it is fixed for the model's life.
    geo: Option<SharedResolver>,
}

impl PartialEq for ConnectionFilter {
    fn eq(&self, other: &Self) -> bool {
        self.geo_scope == other.geo_scope
            && self.query == other.query
            && self.pending_only == other.pending_only
            && self.denied_only == other.denied_only
            && self.since_ms == other.since_ms
            && self.needs_attention == other.needs_attention
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

    pub fn prepared_only(mut self) -> Self {
        self.prepared_geo = Some(Arc::new(HashMap::new()));
        self
    }
    pub fn merge_prepared_geo(&mut self, facts: HashMap<String, Resolved>) {
        Arc::make_mut(
            self.prepared_geo
                .get_or_insert_with(|| Arc::new(HashMap::new())),
        )
        .extend(
            facts
                .into_iter()
                .map(|(id, resolved)| (id, GeoIdentity::from(resolved))),
        );
    }
    pub fn clear_prepared_geo(&mut self) {
        self.prepared_geo = Some(Arc::new(HashMap::new()));
    }
    pub fn retain_prepared_geo(&mut self, keep: impl Fn(&str) -> bool) {
        if let Some(facts) = &mut self.prepared_geo {
            Arc::make_mut(facts).retain(|id, _| keep(id));
        }
    }
    pub fn geo_scope(&self) -> Option<&GeoScope> {
        self.geo_scope.as_ref()
    }
    pub fn set_geo_scope(&mut self, scope: Option<GeoScope>) {
        self.geo_scope = scope;
    }
    fn resolved<'a>(&'a self, row: &ConnectionRow) -> Option<Cow<'a, GeoIdentity>> {
        if let Some(facts) = &self.prepared_geo {
            return facts.get(&row.id).map(Cow::Borrowed);
        }
        let resolved = self.geo.as_ref().map_or_else(
            || crate::geo::resolver::resolve(&row.dst_ip, None),
            |geo| geo.resolve(&row.dst_ip),
        );
        Some(Cow::Owned(GeoIdentity::from(resolved)))
    }

    /// True when the filter would narrow the list. When inactive, callers can
    /// skip projection work entirely and treat the visible list as identity
    /// over the full store.
    pub fn is_active(&self) -> bool {
        self.geo_scope.is_some()
            || self.pending_only
            || self.denied_only
            || self.since_ms.is_some()
            || !self.terms.is_empty()
            || self.collapses()
    }

    /// Whether handled rows are being collapsed now: the switch is on and
    /// some recommended rule is known. Off while the rules list is empty or
    /// unknown, so the default never narrows the list then (fail open).
    fn collapses(&self) -> bool {
        self.needs_attention
            && self
                .attention
                .as_ref()
                .is_some_and(|facts| facts.collapses_anything())
    }

    pub fn needs_attention(&self) -> bool {
        self.needs_attention
    }

    pub fn set_needs_attention(&mut self, needs_attention: bool) {
        self.needs_attention = needs_attention;
    }

    /// Replace what "handled" reads. Callers recompute the projection: a
    /// facts change never changes the filter's identity.
    pub fn set_attention_facts(&mut self, facts: Arc<AttentionFacts>) {
        self.attention = Some(facts);
    }

    pub fn attention_facts(&self) -> Option<&AttentionFacts> {
        self.attention.as_deref()
    }

    /// Whether `row` is handled, whether or not it is collapsed now.
    pub fn is_handled(&self, row: &ConnectionRow) -> bool {
        self.attention
            .as_ref()
            .is_some_and(|facts| facts.is_handled(row))
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
        if let Some(scope) = &self.geo_scope {
            parts.push(scope.description());
        }
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
        if self.collapses() && self.is_handled(row) {
            return false;
        }
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
        if let Some(scope) = &self.geo_scope {
            if !self
                .resolved(row)
                .is_some_and(|resolved| scope.matches(row, &resolved))
            {
                return false;
            }
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
            Term::Country(code) => self.resolved(row).is_some_and(|resolved| {
                resolved.country_key.strip_prefix("country:") == Some(code.as_str())
            }),
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
mod tests;
