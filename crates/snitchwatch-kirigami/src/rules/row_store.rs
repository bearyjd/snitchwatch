//! Pure, Qt-free store for the Rules tab's flat rule list (Task 10).
//!
//! This re-homes `web/js/rules.js`'s rule-list handling into Rust. The bridge
//! emits the typed [`ServerMessage::SetRules`] / [`ServerMessage::UpdateRules`]
//! variants; unlike the Blocklists WS messages, the per-rule payload itself is
//! an untyped `serde_json::Value` (see `ws_messages.rs` doc comment — the exact
//! wire shape wasn't pinned down for rules the way `BlocklistSummary` was for
//! blocklists). [`Rule`] is the tolerant, `#[serde(default)]`-backed shape this
//! store deserializes each value into; entries that fail to parse are dropped
//! rather than panicking or poisoning the whole list.
//!
//! The bridge installs each subscribed blocklist as one `lists.*` deny rule
//! per list kind, named `z00-blocklist:<list_id>:<kind>` (issue #45; see
//! `snitchwatch_bridge::blocklists::materializer`). [`Rule::source`] detects
//! that band by name prefix so the Rules tab can render blocklist-sourced
//! rules distinctly (and grouped) instead of mixing them in with user rules —
//! the list's hosts are shown on the Blocklists tab, so this view
//! intentionally does not repeat them. The bridge marks these rules
//! read-only and not deletable ("Managed on the Blocklists page").
//!
//! Rule state changes are whole-list replaces/upserts, not a hot per-row
//! stream (mirrors the Blocklists store's reasoning) — this store reports a
//! single "did anything change" boolean and the model wrapper brackets each
//! apply with `beginResetModel`/`endResetModel`.

use serde::{Deserialize, Serialize};

use snitchwatch_bridge::ws_messages::ServerMessage;

/// The `z00-blocklist:<list_id>:<kind>` filename band a subscribed
/// blocklist's deny rules fall in (see
/// `snitchwatch_bridge::blocklists::materializer::list_rule_name`).
const BLOCKLIST_RULE_NAME_PREFIX: &str = "z00-blocklist:";

/// The legacy `900-blocklist:` band emitted by pre-migration builds. Still
/// recognized so that, during a migration window, a not-yet-purged old-band
/// deny surfaced by a stale daemon renders in the Rules tab as blocklist-
/// sourced (grouped/muted) rather than masquerading as a user rule. Once the
/// bridge has refreshed every subscription these no longer exist; see the
/// migration note in `snitchwatch_bridge::blocklists::materializer`.
const LEGACY_BLOCKLIST_RULE_NAME_PREFIX: &str = "900-blocklist:";

fn default_enabled() -> bool {
    true
}

/// Tolerant, wire-shape-agnostic view of one opensnitchd rule as carried by
/// `SetRules`/`UpdateRules`'s untyped `serde_json::Value` payload. Missing
/// fields default rather than fail parsing, since the exact shape is still in
/// flux upstream (bridge live-wiring is a later task).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub operator: serde_json::Value,
    /// opensnitchd's `Rule.precedence` — this rule is evaluated ahead of
    /// non-precedence rules, i.e. it can decide traffic another rule would
    /// otherwise match.
    ///
    /// Carried here purely so it survives a round trip. `setEnabled` sends
    /// the whole rule back as a `CHANGE_RULE`, and the daemon's handler does a
    /// wholesale `Replace` — so a field this struct drops is a field the next
    /// toggle silently clears on the daemon. For `precedence` that would
    /// quietly change which rule wins for unrelated traffic. Nothing in the UI
    /// edits it; it is round-trip ballast on purpose.
    pub precedence: bool,
    /// opensnitchd's `Rule.nolog` — suppress logging for matches of this rule.
    /// Same round-trip-ballast rationale as [`Self::precedence`].
    pub nolog: bool,
    /// When the daemon created the rule, in Unix seconds. The daemon never
    /// reports 0: a rule file whose `created` it can't parse is given the
    /// time it was loaded (`rule.go` `Serialize`). 0 here means the bridge
    /// predates the field. Display data only, never sent back.
    #[serde(skip_serializing)]
    pub created: i64,
    /// The bridge's display form of `name`, with bidi overrides and
    /// zero-width characters removed (issue #48). Display only: `name` is the
    /// rule's identity in every command, so this is never sent back.
    #[serde(skip_serializing)]
    pub display_name: Option<String>,
    /// Set by the bridge when Snitchwatch can't edit this rule (a name or
    /// conditions the bridge refuses to send back to the daemon); a
    /// plain-language reason for the user. The row stays visible — the
    /// daemon still enforces it — but no change is emitted for it; see
    /// [`Self::deletable`] for Delete. Never sent back.
    #[serde(skip_serializing)]
    pub read_only_reason: Option<String>,
    /// Set by the bridge: whether Snitchwatch may delete this rule. A rule
    /// read-only only for its conditions still can (a delete names the rule
    /// and nothing else); one read-only for its name can't. `None` from a
    /// bridge that predates the field. Never sent back.
    #[serde(skip_serializing)]
    pub deletable: Option<bool>,
    /// Set by the bridge: whether Snitchwatch may turn this rule on or off.
    /// A recommended background-service rule (prompt-slot D) is read-only
    /// but toggleable. `None` from an older bridge. Never sent back.
    #[serde(skip_serializing)]
    pub toggleable: Option<bool>,
}

/// Where a rule originated: authored directly by the user, or installed for
/// a subscribed blocklist (the `z00-blocklist:<id>:` band, or the legacy
/// `900-blocklist:<id>:` band during a migration window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleSource {
    User,
    Blocklist { list_id: String },
}

impl Rule {
    /// What to show for this rule's name; see [`Self::display_name`]. Falls
    /// back to `name` only for a bridge that predates the field.
    pub fn shown_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only_reason.is_some()
    }

    /// See [`Self::deletable`]; an older bridge's rule is deletable unless
    /// read-only.
    pub fn can_delete(&self) -> bool {
        self.deletable.unwrap_or(!self.is_read_only())
    }

    /// See [`Self::toggleable`]; an older bridge's rule is toggleable unless
    /// read-only.
    pub fn can_toggle(&self) -> bool {
        self.toggleable.unwrap_or(!self.is_read_only())
    }

    /// Classify this rule's source by its `name` prefix. Recognizes both the
    /// current `z00-blocklist:` band and the legacy `900-blocklist:` band (see
    /// [`LEGACY_BLOCKLIST_RULE_NAME_PREFIX`]) so migration-window rules still
    /// group correctly.
    pub fn source(&self) -> RuleSource {
        let rest = self
            .name
            .strip_prefix(BLOCKLIST_RULE_NAME_PREFIX)
            .or_else(|| self.name.strip_prefix(LEGACY_BLOCKLIST_RULE_NAME_PREFIX));
        match rest {
            Some(rest) => RuleSource::Blocklist {
                list_id: rest.split(':').next().unwrap_or("").to_string(),
            },
            None => RuleSource::User,
        }
    }

    pub fn is_blocklist_sourced(&self) -> bool {
        matches!(self.source(), RuleSource::Blocklist { .. })
    }

    /// Normalized to exactly `"allow"` or `"deny"` (opensnitchd's `reject`
    /// action, if ever encountered, is folded into `"deny"` for display —
    /// same normalization `web/js/rules.js`'s `normalizedRuleAction` does).
    pub fn normalized_action(&self) -> &'static str {
        if self.action.eq_ignore_ascii_case("allow") {
            "allow"
        } else {
            "deny"
        }
    }

    /// Human-readable summary of the operator/target scope, e.g.
    /// `"process.path = /usr/bin/firefox"` or, for a compound `list`
    /// operator, each child joined with `" AND "`. Returns an empty string
    /// for a shape this can't recognize rather than guessing.
    pub fn operator_summary(&self) -> String {
        summarize_operator(&self.operator)
    }
}

fn summarize_operator(value: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = value else {
        return String::new();
    };

    // Externally-tagged enum wrapper, e.g. {"simple": {...}} / {"list": {...}}
    // — unwrap the single variant key and recurse into its payload.
    if map.len() == 1 {
        if let Some(inner) = map.values().next() {
            if inner.get("operand").is_some() || inner.get("operands").is_some() {
                return summarize_operator(inner);
            }
        }
    }

    if let Some(operands) = map.get("operands").and_then(|o| o.as_array()) {
        return operands
            .iter()
            .map(summarize_operator)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" AND ");
    }

    let operand = map.get("operand").and_then(|v| v.as_str()).unwrap_or("");
    let data = map.get("data").and_then(|v| v.as_str()).unwrap_or("");
    if operand.is_empty() && data.is_empty() {
        String::new()
    } else {
        format!("{operand} = {data}")
    }
}

/// Ordered, flat list of rules backing the Rules tab. The server already
/// sends rules in evaluation order (opensnitchd evaluates alphabetically by
/// filename and stops at the first match — see the design doc's "Rule
/// precedence" section), so a rule's index in this store *is* its precedence
/// position; no separate numeric field is tracked.
#[derive(Debug, Default)]
pub struct RulesStore {
    rules: Vec<Rule>,
}

impl RulesStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn row(&self, index: usize) -> Option<&Rule> {
        self.rules.get(index)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.name == name)
    }

    /// Position of the rule named `name`, or `None` if unknown. Since a
    /// rule's index in this store *is* its precedence position (see the
    /// struct doc comment), this doubles as "where does this rule sit in
    /// evaluation order" — used by `RulesModel::select_rule_by_name` to jump
    /// the Rules tab to a connection's matched rule.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.rules.iter().position(|r| r.name == name)
    }

    /// Apply one bridge message. Returns `true` if the rule list changed. The
    /// bridge sends the whole list again after every rule command, so the
    /// same list is common and is not a change.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetRules { rules } => {
                let rules: Vec<Rule> = rules
                    .iter()
                    .filter_map(|v| serde_json::from_value::<Rule>(v.clone()).ok())
                    .collect();
                let changed = rules != self.rules;
                if changed {
                    self.rules = rules;
                }
                changed
            }
            ServerMessage::UpdateRules { rules } => {
                let mut changed = false;
                for v in rules {
                    if let Ok(rule) = serde_json::from_value::<Rule>(v.clone()) {
                        changed |= self.upsert(rule);
                    }
                }
                changed
            }
            _ => false,
        }
    }

    fn upsert(&mut self, rule: Rule) -> bool {
        match self.rules.iter_mut().find(|r| r.name == rule.name) {
            Some(existing) if *existing == rule => false,
            Some(existing) => {
                *existing = rule;
                true
            }
            None => {
                self.rules.push(rule);
                true
            }
        }
    }

    /// Whether a rule may be deleted from Snitchwatch: known and
    /// [`Rule::can_delete`].
    pub fn is_deletable(&self, name: &str) -> bool {
        self.find_by_name(name).is_some_and(Rule::can_delete)
    }

    /// Build the full rule payload setting `name`'s `enabled` flag to
    /// `enabled`, with every other field preserved — ready for the model
    /// wrapper to wrap in a `ClientMessage::UpdateRule`. Returns `None` if
    /// `name` isn't known.
    ///
    /// Takes the desired value rather than flipping the stored one: until
    /// the bridge's next `SetRules` arrives, the store still holds the old
    /// value, so a quick second click would otherwise send the same change
    /// twice and leave the rule inverted from what the switch shows (#48).
    /// Also `None` for a rule the bridge says can't be toggled: no command
    /// is built for it.
    pub fn rule_json_with_enabled(&self, name: &str, enabled: bool) -> Option<serde_json::Value> {
        let mut updated = self.find_by_name(name).filter(|r| r.can_toggle())?.clone();
        updated.enabled = enabled;
        serde_json::to_value(&updated).ok()
    }
}

/// JSON shape [`found_rule_json`] returns — the same fields
/// `RulesPage.qml`'s inspector already has properties for, so
/// `RulesModel::select_rule_by_name`'s QML caller can `JSON.parse` this
/// straight into `inspect*` and open the sheet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FoundRule<'a> {
    name: &'a str,
    display_name: &'a str,
    read_only_reason: &'a str,
    deletable: bool,
    toggleable: bool,
    enabled: bool,
    action: &'static str,
    duration: &'a str,
    operator_summary: String,
    /// The rule's position in evaluation order (0-based), i.e. its index in
    /// this store — see the struct doc comment on why that index *is* the
    /// precedence position.
    precedence: usize,
    source: &'static str,
    blocklist_id: String,
}

/// Look up `name` in `store` and, if found, serialize it to the JSON shape
/// [`FoundRule`] describes for the "Show rule" jump (see
/// `RulesModel::select_rule_by_name`). `None` when no rule by that name is
/// currently known.
pub fn found_rule_json(store: &RulesStore, name: &str) -> Option<String> {
    let idx = store.index_of(name)?;
    let rule = store.row(idx)?;
    let (source, blocklist_id) = match rule.source() {
        RuleSource::User => ("user", String::new()),
        RuleSource::Blocklist { list_id } => ("blocklist", list_id),
    };
    let found = FoundRule {
        name: &rule.name,
        display_name: rule.shown_name(),
        read_only_reason: rule.read_only_reason.as_deref().unwrap_or_default(),
        deletable: rule.can_delete(),
        toggleable: rule.can_toggle(),
        enabled: rule.enabled,
        action: rule.normalized_action(),
        duration: &rule.duration,
        operator_summary: rule.operator_summary(),
        precedence: idx,
        source,
        blocklist_id,
    };
    serde_json::to_string(&found).ok()
}

#[cfg(test)]
#[path = "row_store_toggle_tests.rs"]
mod toggle_tests;

#[cfg(test)]
#[path = "row_store_tests.rs"]
mod tests;
