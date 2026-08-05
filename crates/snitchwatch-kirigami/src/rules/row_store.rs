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
//! Per the design doc
//! (`docs/superpowers/specs/2026-04-10-snitchwatch-design.md`, "The blocklist
//! verdict type"), the bridge materializes every subscribed blocklist entry as
//! a deny rule named `z00-blocklist:<list_id>:<seq>-<host>` (see
//! `snitchwatch_bridge::blocklists::materializer`). [`Rule::source`] detects
//! that band by name prefix so the Rules tab can render blocklist-sourced
//! rules distinctly (and grouped) instead of mixing them in with user rules —
//! the same underlying deny rules are already shown in full on the Blocklists
//! tab, so this view intentionally does not repeat their per-host detail.
//!
//! Rule state changes are whole-list replaces/upserts, not a hot per-row
//! stream (mirrors the Blocklists store's reasoning) — this store reports a
//! single "did anything change" boolean and the model wrapper brackets each
//! apply with `beginResetModel`/`endResetModel`.

use serde::{Deserialize, Serialize};

use snitchwatch_bridge::ws_messages::ServerMessage;

/// The `z00-blocklist:<list_id>:<seq>-<host>` filename band a subscribed
/// blocklist's materialized deny rules fall in (see
/// `snitchwatch_bridge::blocklists::materializer::materialize_entry`).
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
    /// Carried here purely so it survives a round trip. `toggleEnabled` sends
    /// the whole rule back as a `CHANGE_RULE`, and the daemon's handler does a
    /// wholesale `Replace` — so a field this struct drops is a field the next
    /// toggle silently clears on the daemon. For `precedence` that would
    /// quietly change which rule wins for unrelated traffic. Nothing in the UI
    /// edits it; it is round-trip ballast on purpose.
    pub precedence: bool,
    /// opensnitchd's `Rule.nolog` — suppress logging for matches of this rule.
    /// Same round-trip-ballast rationale as [`Self::precedence`].
    pub nolog: bool,
}

/// Where a rule originated: authored directly by the user, or materialized
/// from a subscribed blocklist entry (the `z00-blocklist:<id>:` band, or the
/// legacy `900-blocklist:<id>:` band during a migration window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleSource {
    User,
    Blocklist { list_id: String },
}

impl Rule {
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

    /// Apply one bridge message. Returns `true` if the rule list changed (the
    /// model wrapper resets on `true`).
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::SetRules { rules } => {
                self.rules = rules
                    .iter()
                    .filter_map(|v| serde_json::from_value::<Rule>(v.clone()).ok())
                    .collect();
                true
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
            Some(existing) => {
                *existing = rule;
            }
            None => self.rules.push(rule),
        }
        true
    }

    /// Build the full rule payload for toggling `name`'s `enabled` flag, with
    /// every other field preserved — ready for the model wrapper to wrap in a
    /// `ClientMessage::UpdateRule`. Returns `None` if `name` isn't known.
    pub fn toggled_rule_json(&self, name: &str) -> Option<serde_json::Value> {
        let rule = self.find_by_name(name)?;
        let mut updated = rule.clone();
        updated.enabled = !updated.enabled;
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
mod tests {
    use super::*;

    fn rule(name: &str, enabled: bool, action: &str) -> Rule {
        Rule {
            name: name.to_string(),
            enabled,
            action: action.to_string(),
            duration: "always".to_string(),
            description: String::new(),
            operator: serde_json::json!({"operand": "dest.host", "data": "example.com"}),
            precedence: false,
            nolog: false,
        }
    }

    fn names(store: &RulesStore) -> Vec<String> {
        store.rules().iter().map(|r| r.name.clone()).collect()
    }

    #[test]
    fn set_rules_replaces_the_list() {
        let mut s = RulesStore::new();
        assert!(s.apply(&ServerMessage::SetRules {
            rules: vec![
                serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
                serde_json::to_value(rule("z00-blocklist:ads:0001-x.example", true, "deny"))
                    .unwrap(),
            ],
        }));
        assert_eq!(
            names(&s),
            vec!["899-firefox", "z00-blocklist:ads:0001-x.example"]
        );

        // A second SetRules replaces wholesale.
        assert!(s.apply(&ServerMessage::SetRules {
            rules: vec![serde_json::to_value(rule("999-other", true, "deny")).unwrap()],
        }));
        assert_eq!(names(&s), vec!["999-other"]);
    }

    #[test]
    fn update_rules_upserts_by_name() {
        let mut s = RulesStore::new();
        s.apply(&ServerMessage::SetRules {
            rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
        });

        // Update existing.
        assert!(s.apply(&ServerMessage::UpdateRules {
            rules: vec![serde_json::to_value(rule("899-firefox", false, "allow")).unwrap()],
        }));
        assert!(!s.find_by_name("899-firefox").unwrap().enabled);

        // Insert new.
        assert!(s.apply(&ServerMessage::UpdateRules {
            rules: vec![serde_json::to_value(rule("999-new", true, "deny")).unwrap()],
        }));
        assert_eq!(names(&s), vec!["899-firefox", "999-new"]);
    }

    #[test]
    fn unrelated_message_does_not_change_rules() {
        let mut s = RulesStore::new();
        assert!(!s.apply(&ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn malformed_rule_values_are_dropped_not_fatal() {
        let mut s = RulesStore::new();
        assert!(s.apply(&ServerMessage::SetRules {
            rules: vec![
                serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
                serde_json::Value::Null,
                serde_json::json!("not an object"),
            ],
        }));
        // Null/string entries are not a struct shape at all, so they fail to
        // deserialize and are dropped by `filter_map` — only the well-formed
        // object survives. Malformed entries never panic the store.
        assert_eq!(names(&s), vec!["899-firefox"]);
    }

    #[test]
    fn user_rule_source_is_user() {
        let r = rule("899-firefox-allow-out", true, "allow");
        assert_eq!(r.source(), RuleSource::User);
        assert!(!r.is_blocklist_sourced());
    }

    #[test]
    fn blocklist_band_name_detected_by_prefix() {
        let r = rule(
            "z00-blocklist:stevenblack:0001-doubleclick.net",
            true,
            "deny",
        );
        assert_eq!(
            r.source(),
            RuleSource::Blocklist {
                list_id: "stevenblack".to_string()
            }
        );
        assert!(r.is_blocklist_sourced());
    }

    #[test]
    fn legacy_blocklist_band_name_still_detected() {
        // Migration-window tolerance: a stale pre-migration daemon may still
        // surface a "900-blocklist:" rule before the bridge purges it. It must
        // still group as blocklist-sourced, not masquerade as a user rule.
        let r = rule(
            "900-blocklist:stevenblack:0001-doubleclick.net",
            true,
            "deny",
        );
        assert_eq!(
            r.source(),
            RuleSource::Blocklist {
                list_id: "stevenblack".to_string()
            }
        );
        assert!(r.is_blocklist_sourced());
    }

    #[test]
    fn normalized_action_folds_unknown_to_deny() {
        assert_eq!(rule("r", true, "allow").normalized_action(), "allow");
        assert_eq!(rule("r", true, "ALLOW").normalized_action(), "allow");
        assert_eq!(rule("r", true, "deny").normalized_action(), "deny");
        assert_eq!(rule("r", true, "reject").normalized_action(), "deny");
        assert_eq!(rule("r", true, "").normalized_action(), "deny");
    }

    #[test]
    fn operator_summary_simple_shape() {
        let r = rule("r", true, "deny");
        assert_eq!(r.operator_summary(), "dest.host = example.com");
    }

    #[test]
    fn operator_summary_tagged_wrapper_shape() {
        let mut r = rule("r", true, "deny");
        r.operator = serde_json::json!({
            "simple": {"operand": "process.path", "data": "/usr/bin/firefox"}
        });
        assert_eq!(r.operator_summary(), "process.path = /usr/bin/firefox");
    }

    #[test]
    fn operator_summary_list_shape_joins_children() {
        let mut r = rule("r", true, "deny");
        r.operator = serde_json::json!({
            "operands": [
                {"operand": "process.path", "data": "/usr/bin/firefox"},
                {"simple": {"operand": "dest.port", "data": "443"}}
            ]
        });
        assert_eq!(
            r.operator_summary(),
            "process.path = /usr/bin/firefox AND dest.port = 443"
        );
    }

    #[test]
    fn operator_summary_unrecognized_shape_is_empty() {
        let mut r = rule("r", true, "deny");
        r.operator = serde_json::json!("garbage");
        assert_eq!(r.operator_summary(), "");
        r.operator = serde_json::Value::Null;
        assert_eq!(r.operator_summary(), "");
    }

    #[test]
    fn toggled_rule_json_flips_enabled_and_preserves_other_fields() {
        let mut s = RulesStore::new();
        s.apply(&ServerMessage::SetRules {
            rules: vec![serde_json::to_value(rule("899-firefox", true, "allow")).unwrap()],
        });
        let toggled = s.toggled_rule_json("899-firefox").expect("rule known");
        assert_eq!(toggled["enabled"], false);
        assert_eq!(toggled["name"], "899-firefox");
        assert_eq!(toggled["action"], "allow");
    }

    /// A toggle is sent to the daemon as a `CHANGE_RULE` carrying the whole
    /// rule, and the daemon's handler does a wholesale `Replace`. So any field
    /// this store drops on ingest is a field the next toggle silently clears on
    /// the daemon — and for `precedence` that quietly changes which rule wins
    /// for traffic the user wasn't touching.
    ///
    /// This is a real regression: `precedence`/`nolog` were absent from this
    /// struct when the CHANGE_RULE path was first wired, so every toggle reset
    /// them to false.
    #[test]
    fn toggling_preserves_precedence_and_nolog_through_the_wire() {
        let mut s = RulesStore::new();
        s.apply(&ServerMessage::SetRules {
            rules: vec![serde_json::json!({
                "name": "010-priority-allow",
                "enabled": true,
                "action": "allow",
                "duration": "always",
                "description": "",
                "operator": {"operand": "dest.host", "data": "example.com"},
                "precedence": true,
                "nolog": true,
            })],
        });

        // Ingest must not drop them...
        let ingested = s.find_by_name("010-priority-allow").expect("rule known");
        assert!(ingested.precedence, "precedence lost on SetRules ingest");
        assert!(ingested.nolog, "nolog lost on SetRules ingest");

        // ...and the JSON sent back to the daemon must still carry them.
        let toggled = s
            .toggled_rule_json("010-priority-allow")
            .expect("rule known");
        assert_eq!(toggled["enabled"], false, "the toggle itself must apply");
        assert_eq!(
            toggled["precedence"], true,
            "toggling a rule must not clear its precedence on the daemon"
        );
        assert_eq!(
            toggled["nolog"], true,
            "toggling a rule must not clear its nolog flag on the daemon"
        );
    }

    #[test]
    fn toggled_rule_json_unknown_name_is_none() {
        let s = RulesStore::new();
        assert!(s.toggled_rule_json("nope").is_none());
    }

    #[test]
    fn found_rule_json_returns_the_rule_shape_for_a_known_name() {
        let mut s = RulesStore::new();
        s.apply(&ServerMessage::SetRules {
            rules: vec![
                serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
                serde_json::to_value(rule("z00-blocklist:ads:0001-x.example", true, "deny"))
                    .unwrap(),
            ],
        });
        let json = found_rule_json(&s, "z00-blocklist:ads:0001-x.example").expect("known rule");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "z00-blocklist:ads:0001-x.example");
        assert_eq!(parsed["precedence"], 1);
        assert_eq!(parsed["source"], "blocklist");
        assert_eq!(parsed["blocklistId"], "ads");
        assert_eq!(parsed["action"], "deny");
    }

    #[test]
    fn found_rule_json_is_none_for_an_unknown_name() {
        let s = RulesStore::new();
        assert!(found_rule_json(&s, "nope").is_none());
    }

    #[test]
    fn index_of_finds_the_rules_precedence_position() {
        let mut s = RulesStore::new();
        s.apply(&ServerMessage::SetRules {
            rules: vec![
                serde_json::to_value(rule("899-firefox", true, "allow")).unwrap(),
                serde_json::to_value(rule("z00-blocklist:ads:0001-x.example", true, "deny"))
                    .unwrap(),
            ],
        });
        assert_eq!(s.index_of("899-firefox"), Some(0));
        assert_eq!(s.index_of("z00-blocklist:ads:0001-x.example"), Some(1));
        assert_eq!(s.index_of("nope"), None);
    }
}
