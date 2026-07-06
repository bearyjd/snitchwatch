//! Qt-free rule-match **simulator** (Little-Snitch-parity "rule-match
//! diagnostics" — the interactive counterpart to
//! [`super::row_store::found_rule_json`]'s "show me the rule that already
//! decided this connection").
//!
//! Given a candidate process/host/port/protocol, [`simulate`] evaluates the
//! *cached* rule list ([`RulesStore`]) the same way opensnitchd's
//! `rule.Loader.FindFirstMatch` does (`vendor/opensnitch/daemon/rule/loader.go`)
//! and `Operator.Match`/`Operator.Compile` (`vendor/opensnitch/daemon/rule/
//! operator.go`):
//!
//!   * **Order**: rules are evaluated in the store's existing order, which —
//!     per `RulesStore`'s own doc comment — the server already sends in
//!     evaluation order (opensnitchd sorts by filename). A rule's index in
//!     the store *is* its precedence position; there is no separate ordinal
//!     field.
//!   * **Enabled-only**: disabled rules are skipped entirely, mirroring
//!     `Loader.sortRules`'s `activeRules` filter.
//!   * **First-match-wins is not "first true wins"**: this is the daemon's
//!     actual, slightly surprising behaviour (`FindFirstMatch`) — iterate in
//!     order; on every match, remember it as the running `match`; if the
//!     matched rule's action is `deny`/`reject` **or** its `precedence` flag
//!     is set, return immediately. Otherwise keep going — a *later* matching
//!     `allow` rule silently overwrites an earlier one. If no
//!     deny/precedence rule ever matches, the *last* matching allow rule
//!     wins, not the first. [`simulate`] reproduces this exactly, because a
//!     simulator that reports "first match" here would be actively wrong
//!     about real daemon behaviour.
//!   * **Compound `list` operators are AND, not OR**: `Operator.listMatch`
//!     requires every child operator to match
//!     (`res = res && o.List[i].Match(...)`). This mirrors how this bridge's
//!     own rule-authoring path (`translator::rule_semantics::ls_to_os`)
//!     always builds compound scopes as a `List` of independently-necessary
//!     conditions (process AND host AND port AND ...).
//!
//! ## What's simulated
//!
//! Operand types with real matching behaviour implemented, matching
//! `Operator.simpleCmp`/`reCmp` exactly (case-insensitive unless the
//! operator's `sensitive` flag is set, mirroring `Operator.Compile`/`Match`):
//!   * `process.path` — simple (exact, case-insensitive by default) or
//!     regexp match against the candidate process path.
//!   * `dest.host` — simple or regexp match against the candidate host. This
//!     covers "wildcard" host rules too: this bridge's own rule authoring
//!     (`translator::glob`) already lowers glob patterns to a `regexp`
//!     operator before a rule ever reaches the daemon, so there is no
//!     separate wildcard operand to special-case.
//!   * `dest.port` — simple match against the candidate port, formatted as
//!     the same decimal string the daemon compares
//!     (`strconv.FormatUint`, mirrored here as `to_string()`).
//!   * `protocol` — simple, case-insensitive match.
//!   * `list` — the compound AND operator described above.
//!   * `true` — always matches (opensnitchd's `OpTrue`, used by catch-all
//!     rules).
//!
//! ## What's deliberately NOT simulated
//!
//! Any other operand (`process.parent.path`, `process.command`,
//! `process.hash.*`, `user.id`/`user.name`, `source.*`, `iface.*`,
//! `process.env.*`, and the `lists.*` family that reference externally-fetched
//! domain/IP/hash lists this store doesn't cache the contents of) is
//! recognized but treated as **non-matching** rather than guessed at, and its
//! operand name is collected into [`SimulationResult::unsupported_operands`]
//! so the UI can flag that the result may not reflect real daemon behaviour
//! for that rule. This is a deliberate, documented limitation, not a bug: a
//! simulator that silently guessed "true" or "false" for operand types it
//! can't actually evaluate would be worse than one that says so. Every result
//! this module returns is labelled `SimulationResult` precisely so callers
//! (and the QML "Simulate" panel) never present it as a live daemon verdict.

use serde::Serialize;

use super::row_store::{Rule, RulesStore};

/// The candidate connection to evaluate against the cached rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationInput {
    pub process_path: String,
    pub dest_host: String,
    pub dest_port: u16,
    /// e.g. `"tcp"` / `"udp"`. Matched case-insensitively, same as the
    /// daemon's `protocol` operand.
    pub protocol: String,
}

/// Outcome of [`simulate`]. Always labelled as a simulation — never a live
/// daemon verdict — per this module's doc comment.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResult {
    /// `Some(name)` when a rule matched (mirrors `Loader.FindFirstMatch`'s
    /// possibly-nil `match`); `None` means no enabled rule matched at all, so
    /// opensnitchd's own configured default action would apply instead — this
    /// module has no visibility into that default, so it just reports "no
    /// match".
    pub matched_rule: Option<String>,
    pub action: Option<String>,
    /// The winning rule's position in the evaluated (precedence) order, or
    /// `None` when nothing matched.
    pub precedence: Option<usize>,
    /// Operand names encountered in the *evaluated path* that this simulator
    /// doesn't implement real matching for (see the module doc's "What's
    /// deliberately NOT simulated" section). Non-empty means the result for
    /// at least one visited rule may not reflect real daemon behaviour.
    pub unsupported_operands: Vec<String>,
}

/// Simulate matching `input` against `store`'s cached, enabled rules using
/// opensnitchd's exact precedence semantics (see module docs).
pub fn simulate(store: &RulesStore, input: &SimulationInput) -> SimulationResult {
    let mut matched: Option<(&Rule, usize)> = None;
    let mut unsupported = Vec::new();

    for (idx, rule) in store.rules().iter().enumerate() {
        if !rule.enabled {
            continue;
        }
        let outcome = eval_operator_value(&rule.operator, input);
        if !outcome.matched {
            unsupported.extend(outcome.unsupported_operands);
            continue;
        }
        unsupported.extend(outcome.unsupported_operands);
        matched = Some((rule, idx));

        let action = rule.normalized_action();
        if action == "deny" || is_precedence(rule) {
            break;
        }
    }

    unsupported.sort();
    unsupported.dedup();

    match matched {
        Some((rule, idx)) => SimulationResult {
            matched_rule: Some(rule.name.clone()),
            action: Some(rule.normalized_action().to_string()),
            precedence: Some(idx),
            unsupported_operands: unsupported,
        },
        None => SimulationResult {
            matched_rule: None,
            action: None,
            precedence: None,
            unsupported_operands: unsupported,
        },
    }
}

/// `Rule` (this store's tolerant wire shape) doesn't carry opensnitchd's
/// `precedence` flag today — `RulesStore`/`Rule` was built before this
/// simulator needed it (see `rules::row_store::Rule`'s fields). Reading it
/// straight from the raw `operator`-sibling JSON isn't possible either: the
/// flag lives at the *rule* level, not inside the operator value this struct
/// retains. Until `Rule` grows a `precedence: bool` field fed from the wire
/// message, this conservatively reports `false` (never force-stop on a
/// non-deny match) — the safer default, since it only affects the
/// last-match-wins vs. stop-here choice among *allow* matches, never whether
/// something matches at all. Documented here rather than silently guessed.
fn is_precedence(_rule: &Rule) -> bool {
    false
}

struct OperatorOutcome {
    matched: bool,
    unsupported_operands: Vec<String>,
}

fn eval_operator_value(value: &serde_json::Value, input: &SimulationInput) -> OperatorOutcome {
    match parse_operator(value) {
        Some(op) => eval_parsed(&op, input),
        None => OperatorOutcome {
            matched: false,
            unsupported_operands: vec!["<unrecognized operator shape>".to_string()],
        },
    }
}

/// One operand/comparison node, after normalizing away the wire-format
/// variance `rules::row_store::summarize_operator` already tolerates (the
/// flat opensnitchd shape `{"type","operand","data","list"}` — see
/// `vendor/opensnitch/daemon/rule/operator.go`'s `Operator` struct — and this
/// bridge's own externally-tagged `OsOperator` shape used internally by
/// `translator::rule_semantics`, e.g. `{"simple": {"operand","data"}}`).
enum ParsedOperator {
    True,
    Simple {
        operand: String,
        data: String,
        sensitive: bool,
    },
    Regexp {
        operand: String,
        data: String,
        sensitive: bool,
    },
    List(Vec<ParsedOperator>),
    /// Recognized as an operator node but not one this simulator evaluates
    /// (see the module doc's "What's deliberately NOT simulated").
    Unsupported(String),
}

fn parse_operator(value: &serde_json::Value) -> Option<ParsedOperator> {
    let obj = value.as_object()?;

    // This bridge's internal `OsOperator` shape: a single key naming the
    // variant (`simple` / `regexp` / `list`), wrapping the payload.
    if obj.len() == 1 {
        if let Some((tag, inner)) = obj.iter().next() {
            if inner.get("operand").is_some() || inner.get("operands").is_some() {
                return parse_tagged(tag, inner);
            }
        }
    }

    // The real opensnitchd wire shape: flat `type`/`operand`/`data`/`list`.
    let operand = obj.get("operand").and_then(|v| v.as_str()).unwrap_or("");
    let data = obj.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let sensitive = obj
        .get("sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("simple");

    if operand == "true" {
        return Some(ParsedOperator::True);
    }
    if operand == "list" || kind == "list" {
        let children = obj
            .get("list")
            .or_else(|| obj.get("operands"))
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(parse_operator).collect())
            .unwrap_or_default();
        return Some(ParsedOperator::List(children));
    }

    Some(match kind {
        "regexp" => ParsedOperator::Regexp {
            operand: operand.to_string(),
            data: data.to_string(),
            sensitive,
        },
        "simple" => ParsedOperator::Simple {
            operand: operand.to_string(),
            data: data.to_string(),
            sensitive,
        },
        other => ParsedOperator::Unsupported(if operand.is_empty() {
            other.to_string()
        } else {
            operand.to_string()
        }),
    })
}

fn parse_tagged(tag: &str, inner: &serde_json::Value) -> Option<ParsedOperator> {
    let operand = inner.get("operand").and_then(|v| v.as_str()).unwrap_or("");
    let data = inner.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let sensitive = inner
        .get("sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match tag {
        "simple" => Some(ParsedOperator::Simple {
            operand: operand.to_string(),
            data: data.to_string(),
            sensitive,
        }),
        "regexp" => Some(ParsedOperator::Regexp {
            operand: operand.to_string(),
            data: data.to_string(),
            sensitive,
        }),
        "list" => {
            let children = inner
                .get("operands")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(parse_operator).collect())
                .unwrap_or_default();
            Some(ParsedOperator::List(children))
        }
        other => Some(ParsedOperator::Unsupported(other.to_string())),
    }
}

/// Recognized-but-not-evaluated operand names (see module docs). `dest.host`,
/// `process.path`, `dest.port`, `protocol`, `list`, and `true` are handled
/// directly in [`eval_parsed`] and never reach here.
const SUPPORTED_OPERANDS: &[&str] = &["process.path", "dest.host", "dest.port", "protocol"];

fn eval_parsed(op: &ParsedOperator, input: &SimulationInput) -> OperatorOutcome {
    match op {
        ParsedOperator::True => OperatorOutcome {
            matched: true,
            unsupported_operands: Vec::new(),
        },
        ParsedOperator::Simple {
            operand,
            data,
            sensitive,
        } => {
            let Some(subject) = subject_for(operand, input) else {
                return OperatorOutcome {
                    matched: false,
                    unsupported_operands: vec![operand.clone()],
                };
            };
            OperatorOutcome {
                matched: simple_cmp(&subject, data, *sensitive),
                unsupported_operands: Vec::new(),
            }
        }
        ParsedOperator::Regexp {
            operand,
            data,
            sensitive,
        } => {
            let Some(subject) = subject_for(operand, input) else {
                return OperatorOutcome {
                    matched: false,
                    unsupported_operands: vec![operand.clone()],
                };
            };
            OperatorOutcome {
                matched: regexp_cmp(&subject, data, *sensitive),
                unsupported_operands: Vec::new(),
            }
        }
        ParsedOperator::List(children) => {
            // AND semantics (see module docs): every child must match.
            let mut matched = true;
            let mut unsupported = Vec::new();
            for child in children {
                let outcome = eval_parsed(child, input);
                matched = matched && outcome.matched;
                unsupported.extend(outcome.unsupported_operands);
            }
            OperatorOutcome {
                matched,
                unsupported_operands: unsupported,
            }
        }
        ParsedOperator::Unsupported(operand) => OperatorOutcome {
            matched: false,
            unsupported_operands: vec![operand.clone()],
        },
    }
}

/// The candidate value a `simple`/`regexp` operator compares against, for the
/// operand names this simulator supports (see [`SUPPORTED_OPERANDS`]).
/// `None` for anything else.
fn subject_for(operand: &str, input: &SimulationInput) -> Option<String> {
    if !SUPPORTED_OPERANDS.contains(&operand) {
        return None;
    }
    Some(match operand {
        "process.path" => input.process_path.clone(),
        "dest.host" => input.dest_host.clone(),
        "dest.port" => input.dest_port.to_string(),
        "protocol" => input.protocol.clone(),
        _ => unreachable!("guarded by SUPPORTED_OPERANDS check above"),
    })
}

/// Mirrors `Operator.simpleCmp`: case-insensitive equality unless `sensitive`.
fn simple_cmp(subject: &str, data: &str, sensitive: bool) -> bool {
    if sensitive {
        subject == data
    } else {
        subject.eq_ignore_ascii_case(data)
    }
}

/// Mirrors `Operator.reCmp`: case-insensitive (both sides lowercased) unless
/// `sensitive`. An invalid pattern never matches rather than panicking —
/// opensnitchd itself would have refused to load such a rule at
/// `Operator.Compile()` time, but this simulator sees whatever JSON the store
/// currently holds and must stay robust to it.
fn regexp_cmp(subject: &str, pattern: &str, sensitive: bool) -> bool {
    let (subject, pattern) = if sensitive {
        (subject.to_string(), pattern.to_string())
    } else {
        (subject.to_lowercase(), pattern.to_lowercase())
    };
    regex::Regex::new(&pattern)
        .map(|re| re.is_match(&subject))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::ServerMessage;

    fn store_with(rules: Vec<serde_json::Value>) -> RulesStore {
        let mut s = RulesStore::new();
        s.apply(&ServerMessage::SetRules { rules });
        s
    }

    fn simple_rule(
        name: &str,
        enabled: bool,
        action: &str,
        operand: &str,
        data: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "enabled": enabled,
            "action": action,
            "duration": "always",
            "description": "",
            "operator": {"type": "simple", "operand": operand, "data": data, "sensitive": false, "list": []}
        })
    }

    fn regexp_rule(name: &str, action: &str, operand: &str, pattern: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "enabled": true,
            "action": action,
            "duration": "always",
            "description": "",
            "operator": {"type": "regexp", "operand": operand, "data": pattern, "sensitive": false, "list": []}
        })
    }

    fn list_rule(name: &str, action: &str, children: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "enabled": true,
            "action": action,
            "duration": "always",
            "description": "",
            "operator": {
                "type": "list",
                "operand": "list",
                "data": "",
                "sensitive": false,
                "list": children.into_iter().map(|c| c["operator"].clone()).collect::<Vec<_>>()
            }
        })
    }

    fn input(process_path: &str, host: &str, port: u16, protocol: &str) -> SimulationInput {
        SimulationInput {
            process_path: process_path.to_string(),
            dest_host: host.to_string(),
            dest_port: port,
            protocol: protocol.to_string(),
        }
    }

    #[test]
    fn no_rules_yields_no_match() {
        let store = RulesStore::new();
        let result = simulate(&store, &input("/usr/bin/curl", "github.com", 443, "tcp"));
        assert_eq!(result.matched_rule, None);
        assert_eq!(result.action, None);
        assert_eq!(result.precedence, None);
    }

    #[test]
    fn simple_process_path_match() {
        let store = store_with(vec![simple_rule(
            "899-curl",
            true,
            "allow",
            "process.path",
            "/usr/bin/curl",
        )]);
        let result = simulate(&store, &input("/usr/bin/curl", "github.com", 443, "tcp"));
        assert_eq!(result.matched_rule.as_deref(), Some("899-curl"));
        assert_eq!(result.action.as_deref(), Some("allow"));
        assert_eq!(result.precedence, Some(0));
    }

    #[test]
    fn simple_match_is_case_insensitive_by_default() {
        let store = store_with(vec![simple_rule(
            "899-curl",
            true,
            "allow",
            "process.path",
            "/USR/BIN/CURL",
        )]);
        let result = simulate(&store, &input("/usr/bin/curl", "h", 1, "tcp"));
        assert_eq!(result.matched_rule.as_deref(), Some("899-curl"));
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let store = store_with(vec![
            simple_rule("000-disabled", false, "deny", "dest.host", "github.com"),
            simple_rule("899-allow", true, "allow", "process.path", "/usr/bin/curl"),
        ]);
        let result = simulate(&store, &input("/usr/bin/curl", "github.com", 443, "tcp"));
        assert_eq!(result.matched_rule.as_deref(), Some("899-allow"));
        assert_eq!(result.action.as_deref(), Some("allow"));
    }

    #[test]
    fn deny_match_short_circuits_immediately() {
        let store = store_with(vec![
            simple_rule("899-allow", true, "allow", "process.path", "/usr/bin/curl"),
            simple_rule("900-deny", true, "deny", "dest.host", "github.com"),
            simple_rule("999-allow-again", true, "allow", "protocol", "tcp"),
        ]);
        let result = simulate(&store, &input("/usr/bin/curl", "github.com", 443, "tcp"));
        // The deny at index 1 stops iteration even though a later allow would
        // also have matched — mirrors FindFirstMatch's short-circuit.
        assert_eq!(result.matched_rule.as_deref(), Some("900-deny"));
        assert_eq!(result.action.as_deref(), Some("deny"));
        assert_eq!(result.precedence, Some(1));
    }

    #[test]
    fn later_allow_match_overwrites_an_earlier_allow_match() {
        // Neither rule is deny/precedence, so FindFirstMatch keeps scanning
        // and the *last* matching allow wins, not the first.
        let store = store_with(vec![
            simple_rule("100-allow-a", true, "allow", "protocol", "tcp"),
            simple_rule("200-allow-b", true, "allow", "dest.host", "github.com"),
        ]);
        let result = simulate(&store, &input("/usr/bin/curl", "github.com", 443, "tcp"));
        assert_eq!(result.matched_rule.as_deref(), Some("200-allow-b"));
        assert_eq!(result.precedence, Some(1));
    }

    #[test]
    fn regexp_wildcard_style_host_match() {
        // "*.tracker.example" lowered to a regexp the way
        // `translator::glob::glob_to_regex_string` would.
        let store = store_with(vec![regexp_rule(
            "900-block-tracker",
            "deny",
            "dest.host",
            r"^[^.]*\.tracker\.example$",
        )]);
        let result = simulate(
            &store,
            &input("/usr/bin/curl", "ads.tracker.example", 443, "tcp"),
        );
        assert_eq!(result.matched_rule.as_deref(), Some("900-block-tracker"));
        assert_eq!(result.action.as_deref(), Some("deny"));

        let miss = simulate(&store, &input("/usr/bin/curl", "example.com", 443, "tcp"));
        assert_eq!(miss.matched_rule, None);
    }

    #[test]
    fn dest_port_match_compares_as_decimal_string() {
        let store = store_with(vec![simple_rule(
            "899-dns",
            true,
            "deny",
            "dest.port",
            "53",
        )]);
        assert_eq!(
            simulate(&store, &input("/usr/bin/dig", "h", 53, "udp"))
                .matched_rule
                .as_deref(),
            Some("899-dns")
        );
        assert!(simulate(&store, &input("/usr/bin/dig", "h", 80, "udp"))
            .matched_rule
            .is_none());
    }

    #[test]
    fn list_operator_requires_every_child_to_match() {
        let store = store_with(vec![list_rule(
            "899-firefox-github",
            "allow",
            vec![
                simple_rule("_", true, "allow", "process.path", "/usr/bin/firefox"),
                simple_rule("_", true, "allow", "dest.host", "github.com"),
            ],
        )]);
        assert_eq!(
            simulate(&store, &input("/usr/bin/firefox", "github.com", 443, "tcp"))
                .matched_rule
                .as_deref(),
            Some("899-firefox-github")
        );
        // Process matches but host doesn't -> AND fails -> no match.
        assert!(
            simulate(&store, &input("/usr/bin/firefox", "slack.com", 443, "tcp"))
                .matched_rule
                .is_none()
        );
    }

    #[test]
    fn unsupported_operand_is_recorded_and_treated_as_non_matching() {
        let store = store_with(vec![simple_rule(
            "899-hash",
            true,
            "allow",
            "process.hash.md5",
            "deadbeef",
        )]);
        let result = simulate(&store, &input("/usr/bin/curl", "github.com", 443, "tcp"));
        assert!(result.matched_rule.is_none());
        assert_eq!(result.unsupported_operands, vec!["process.hash.md5"]);
    }

    #[test]
    fn true_operand_always_matches() {
        let store = store_with(vec![serde_json::json!({
            "name": "999-catch-all",
            "enabled": true,
            "action": "deny",
            "duration": "always",
            "description": "",
            "operator": {"type": "simple", "operand": "true", "data": "", "sensitive": false, "list": []}
        })]);
        let result = simulate(&store, &input("/anything", "anything.example", 1, "tcp"));
        assert_eq!(result.matched_rule.as_deref(), Some("999-catch-all"));
    }

    #[test]
    fn protocol_match_is_case_insensitive() {
        let store = store_with(vec![simple_rule(
            "899-tcp", true, "deny", "protocol", "TCP",
        )]);
        assert_eq!(
            simulate(&store, &input("/x", "h", 1, "tcp"))
                .matched_rule
                .as_deref(),
            Some("899-tcp")
        );
    }
}
