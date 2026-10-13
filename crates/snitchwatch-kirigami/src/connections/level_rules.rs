//! Level rule state (Phase 2 step 3): for a node of the connections tree, which
//! rule(s) already exist at its level, without Qt.
//!
//! A tree node is a program (`AnyHost`), a registrable domain
//! (`AnyHostOnDomain`) or a host (`ThisHost`). All three are bound to the
//! program's path. The expected rule is built by the bridge's own
//! `verdict_to_rule`, so this recognises exactly what the GUI and the bridge
//! create, and so the level it reports is the one the bridge really builds:
//! a bare IP, a single-label host or a public suffix degrades to `ThisHost`
//! there, and is reported as [`Level::Host`], never a broader level.
//!
//! A domain row's key is the grouping heuristic's (`grouping::registrable_domain`),
//! not the bridge's. The bridge wildcards a host's leftmost label, so the
//! expected rule is built for the made-up host `x.<key>`. If that degrades
//! and the key has a dot (`co.uk`, `github.io`), no rule can cover the
//! domain ([`Target::too_broad`]) and nothing is created for it.
//!
//! What counts as "a rule at this level": enabled, allow/deny, a user rule
//! (not a blocklist, profile or recommended one), whose operator equals the
//! expected one (`dest.host` compared case-insensitively, as the daemon does;
//! `process.path` exactly). Names offered for deleting are only deletable
//! rules'. Duplicates at one level (say a 5-minute and a forever rule) all
//! count; the first name is the one a delete takes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use snitchwatch_bridge::cache::connections::Verdict;
use snitchwatch_bridge::rule_wire::rule_to_wire;
use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;
use snitchwatch_bridge::translator::verdict::once_rule;
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage, VerdictScope};
use snitchwatch_proto::protocol::Connection;

use crate::make_rule::{ending_of, Ending};
use crate::rules::editor_view::{finished_with, Finished, Wording};
use crate::rules::row_store::{Rule, RuleSource, RulesStore};

/// The tree tier, numbered as the model's `groupLevel` role is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    Program = 0,
    Domain = 1,
    Host = 2,
}

impl Level {
    /// `None` for anything but 0, 1, 2: an unknown level never widens.
    pub fn from_i32(level: i32) -> Option<Self> {
        match level {
            0 => Some(Self::Program),
            1 => Some(Self::Domain),
            2 => Some(Self::Host),
            _ => None,
        }
    }

    /// The bridge scope a rule at this level asks for.
    pub fn scope(self) -> VerdictScope {
        match self {
            Self::Program => VerdictScope::AnyHost,
            Self::Domain => VerdictScope::AnyHostOnDomain,
            Self::Host => VerdictScope::ThisHost,
        }
    }
}

/// A tree node: the program's group key (its executable path when known) and,
/// below the program, the domain's or host's group key.
#[derive(Debug, Clone, Copy)]
pub struct NodeRef<'a> {
    pub program: &'a str,
    pub level: Level,
    pub key: &'a str,
}

/// [`NodeRef`], owned (an in-flight command remembers its node).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeKey {
    pub program: String,
    pub level: Level,
    pub key: String,
}

impl NodeRef<'_> {
    fn to_key(self) -> NodeKey {
        NodeKey {
            program: self.program.to_owned(),
            level: self.level,
            key: if self.level == Level::Program {
                String::new()
            } else {
                self.key.to_owned()
            },
        }
    }
}

/// What a node's button shows (the `levelRuleState` role, as its token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    /// No rule at this level, and none below it.
    None,
    /// An allow rule at this level.
    Allow,
    /// A deny rule at this level.
    Deny,
    /// Allow and deny both at this level, or no rule at this level but
    /// narrower rules for the same program under the node.
    Partial,
    /// A create or delete command for this node is in flight.
    Pending,
}

impl RuleState {
    /// The `levelRuleState` role's string: `none`, `allow`, `deny`,
    /// `partial` or `pending`.
    pub fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Partial => "partial",
            Self::Pending => "pending",
        }
    }
}

/// How a node maps onto the rule the bridge would build.
#[derive(Debug, Clone)]
pub struct Target {
    pub requested: Level,
    /// The level the bridge's rule really has. Never broader than requested.
    pub effective: Level,
    /// The program's path can be bound to (issue #44).
    pub bindable: bool,
    /// A dotted domain key the bridge can't wildcard (a public suffix):
    /// no rule can cover the whole domain.
    pub too_broad: bool,
    /// The expected rule's operator in the wire shape, canonical for
    /// comparison; `None` when no rule can be made.
    pub operator: Option<Value>,
    pub(crate) conn: Connection,
    pub(crate) scope: VerdictScope,
    /// What the made rule's name says in place of a host.
    pub(crate) label: String,
}

fn is_ip(key: &str) -> bool {
    key.parse::<IpAddr>().is_ok()
}

/// The wire operator the bridge builds for `scope` over `conn` (canonical).
fn bridge_operator(conn: &Connection, scope: VerdictScope) -> Value {
    let rule = once_rule(Verdict::Deny, scope, conn, 0);
    canonical(&rule_to_wire(&rule)["operator"])
}

/// `dest.host` data lowercased (the daemon compares it case-insensitively),
/// everything else as is.
fn canonical(operator: &Value) -> Value {
    let mut operator = operator.clone();
    lowercase_hosts(&mut operator);
    operator
}

fn lowercase_hosts(operator: &mut Value) {
    if operator["operand"] == "dest.host" && operator["type"] == "simple" {
        if let Some(data) = operator["data"].as_str() {
            operator["data"] = Value::String(data.to_ascii_lowercase());
        }
    }
    if let Some(members) = operator.get_mut("operands").and_then(Value::as_array_mut) {
        members.iter_mut().for_each(lowercase_hosts);
    }
}

/// The level of a wire operator, from its shape.
fn classify(operator: &Value) -> Level {
    match operator.get("operands").and_then(Value::as_array) {
        Some(members) if members.iter().any(|m| m["type"] == "regexp") => Level::Domain,
        Some(_) => Level::Host,
        None if operator["operand"] == "process.path" => Level::Program,
        None => Level::Host,
    }
}

fn members(operator: &Value) -> impl Iterator<Item = &Value> {
    let list = operator.get("operands").and_then(Value::as_array);
    list.into_iter().flatten().chain(
        list.is_none()
            .then_some(operator)
            .filter(|op| op.get("operand").is_some()),
    )
}

/// The program path a rule's operator binds to.
fn bound_program(operator: &Value) -> Option<&str> {
    members(operator)
        .find(|m| m["operand"] == "process.path")
        .and_then(|m| m["data"].as_str())
}

/// The `dest.host` a host-level rule names.
fn dest_host(operator: &Value) -> Option<String> {
    members(operator)
        .find(|m| m["operand"] == "dest.host" && m["type"] == "simple")
        .and_then(|m| m["data"].as_str())
        .map(str::to_ascii_lowercase)
}

/// Maps `node` onto the rule the bridge would build for it.
pub fn resolve(node: NodeRef<'_>) -> Target {
    let bindable = is_bindable_process_path(node.program);
    let mut conn = Connection {
        process_path: node.program.to_owned(),
        ..Default::default()
    };
    let mut scope = node.level.scope();
    let label = match node.level {
        Level::Program => "any-host".to_owned(),
        _ if is_ip(node.key) => {
            conn.dst_ip = node.key.to_owned();
            node.key.to_owned()
        }
        Level::Host => {
            conn.dst_host = node.key.to_owned();
            node.key.to_owned()
        }
        Level::Domain => {
            // The bridge drops the leftmost label, so the parent is `key`.
            conn.dst_host = format!("x.{}", node.key);
            format!("domain-{}", node.key)
        }
    };
    let mut operator = bridge_operator(&conn, scope);
    let mut label = label;
    let mut effective = if bindable {
        classify(&operator)
    } else {
        node.level
    };
    let mut too_broad = false;
    if node.level == Level::Domain && bindable && effective == Level::Host && !is_ip(node.key) {
        if node.key.contains('.') {
            too_broad = true;
        } else {
            // A single label (`localhost`) is a host of its own.
            conn.dst_host = node.key.to_owned();
            scope = VerdictScope::ThisHost;
            label = node.key.to_owned();
            operator = bridge_operator(&conn, scope);
            effective = Level::Host;
        }
    }
    Target {
        requested: node.level,
        effective,
        bindable,
        too_broad,
        operator: (bindable && !too_broad).then_some(operator),
        conn,
        scope,
        label,
    }
}

/// What a node shows: its state, and the deletable rules at its level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    pub state: RuleState,
    /// Names of the deletable allow/deny rules at the node's level, in the
    /// firewall's order.
    pub names: Vec<String>,
    pub effective: Level,
}

/// What a command in flight is.
#[derive(Debug)]
enum WaitKind {
    Create { operator: Value },
    Delete { name: String },
}

#[derive(Debug)]
struct Wait {
    request_id: String,
    node: NodeKey,
    kind: WaitKind,
    since: Instant,
}

/// A command that ended: for which node, and what to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelFinished {
    pub node: NodeKey,
    /// `Created` means the command succeeded, for a delete too.
    pub ending: Ending,
    pub finished: Finished,
}

/// What [`LevelRules::apply`] changed.
#[derive(Debug, Default)]
pub struct Applied {
    pub rules_changed: bool,
    pub finished: Option<LevelFinished>,
}

const CREATE_WORDING: Wording = Wording {
    saved: "The rule was created.",
    not_saved: "The rule wasn't created: ",
    not_sent: "The rule wasn't sent: ",
    no_daemon: "The firewall service isn't connected, so the rule wasn't sent.",
    unknown: crate::make_rule::NO_ANSWER,
    note_after_saved: true,
};

const DELETE_WORDING: Wording = Wording {
    saved: "The rule was deleted.",
    not_saved: "The rule wasn't deleted: ",
    not_sent: "The delete wasn't sent: ",
    no_daemon: "The firewall service isn't connected, so nothing was deleted.",
    unknown:
        "No answer from the firewall in time. The rule may have been deleted; check the Rules page.",
    note_after_saved: true,
};

/// Past this many remembered targets the memo starts over.
const MAX_MEMO: usize = 20_000;

/// The rules the firewall reports, and the level commands in flight.
#[derive(Debug, Default)]
pub struct LevelRules {
    store: RulesStore,
    waits: Vec<Wait>,
    memo: Mutex<HashMap<NodeKey, Arc<Target>>>,
}

impl LevelRules {
    /// The target of `node`, memoized: the bridge is asked once per node.
    pub fn target(&self, node: NodeRef<'_>) -> Arc<Target> {
        let key = node.to_key();
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if memo.len() >= MAX_MEMO {
            memo.clear();
        }
        Arc::clone(memo.entry(key).or_insert_with(|| Arc::new(resolve(node))))
    }

    /// Takes the rule list from `SetRules`/`UpdateRules`, and ends the
    /// command a `RuleCommandResult` answers.
    pub fn apply(&mut self, msg: &ServerMessage) -> Applied {
        let rules_changed = self.store.apply(msg);
        let finished = match msg {
            ServerMessage::RuleCommandResult {
                request_id,
                outcome,
            } => self.finish(request_id, outcome),
            _ => None,
        };
        Applied {
            rules_changed,
            finished,
        }
    }

    fn finish(&mut self, request_id: &str, outcome: &RuleCommandOutcome) -> Option<LevelFinished> {
        let at = self.waits.iter().position(|w| w.request_id == request_id)?;
        let wait = self.waits.remove(at);
        let wording = match wait.kind {
            WaitKind::Create { .. } => &CREATE_WORDING,
            WaitKind::Delete { .. } => &DELETE_WORDING,
        };
        Some(LevelFinished {
            node: wait.node,
            ending: ending_of(outcome),
            finished: finished_with(outcome, wording),
        })
    }

    /// A create for `node` was sent under `request_id`.
    pub fn begin_create(&mut self, request_id: &str, node: NodeRef<'_>, operator: Value) {
        self.begin(request_id, node, WaitKind::Create { operator });
    }

    /// A delete of the rule `name` at `node` was sent under `request_id`.
    pub fn begin_delete(&mut self, request_id: &str, node: NodeRef<'_>, name: &str) {
        let name = name.to_owned();
        self.begin(request_id, node, WaitKind::Delete { name });
    }

    fn begin(&mut self, request_id: &str, node: NodeRef<'_>, kind: WaitKind) {
        self.waits.push(Wait {
            request_id: request_id.to_owned(),
            node: node.to_key(),
            kind,
            since: Instant::now(),
        });
    }

    /// The command wasn't queued after all: nothing to wait for.
    pub fn abandon(&mut self, request_id: &str) {
        self.waits.retain(|w| w.request_id != request_id);
    }

    /// Ends every command that has waited `after` or more by `now`, as
    /// unknown (the rule may or may not exist).
    pub fn expire(&mut self, now: Instant, after: Duration) -> Vec<LevelFinished> {
        let due = |w: &Wait| now.saturating_duration_since(w.since) >= after;
        self.end_where(due)
    }

    /// Ends every command in flight as unknown: no result can come for a
    /// session that is gone.
    pub fn abandon_all(&mut self) -> Vec<LevelFinished> {
        self.end_where(|_| true)
    }

    fn end_where(&mut self, pick: impl Fn(&Wait) -> bool) -> Vec<LevelFinished> {
        let (gone, kept): (Vec<Wait>, Vec<Wait>) = std::mem::take(&mut self.waits)
            .into_iter()
            .partition(|w| pick(w));
        self.waits = kept;
        gone.into_iter()
            .map(|w| {
                let wording = match w.kind {
                    WaitKind::Create { .. } => &CREATE_WORDING,
                    WaitKind::Delete { .. } => &DELETE_WORDING,
                };
                LevelFinished {
                    node: w.node,
                    ending: Ending::Unknown,
                    finished: finished_with(&RuleCommandOutcome::Timeout, wording),
                }
            })
            .collect()
    }

    /// Whether the rule `name` may be deleted from Snitchwatch.
    pub fn is_deletable(&self, name: &str) -> bool {
        self.store.is_deletable(name)
    }

    /// The state of `node` and the names of its deletable rules.
    pub fn project(&self, node: NodeRef<'_>) -> Projection {
        let target = self.target(node);
        let none = |state| Projection {
            state,
            names: Vec::new(),
            effective: target.effective,
        };
        let Some(expected) = target.operator.as_ref() else {
            return none(RuleState::None);
        };
        let key = node.to_key();
        let mut exact: Vec<&Rule> = Vec::new();
        let mut narrower = false;
        for rule in self.store.rules().iter().filter(|r| counts(r)) {
            let operator = canonical(&rule.operator);
            if &operator == expected {
                exact.push(rule);
            } else if bound_program(&operator) == Some(node.program)
                && is_below(&operator, target.effective, &key.key)
            {
                narrower = true;
            }
        }
        let names: Vec<String> = exact
            .iter()
            .filter(|r| r.can_delete())
            .map(|r| r.name.clone())
            .collect();
        let pending = self.waits.iter().any(|w| {
            w.node == key
                || match &w.kind {
                    WaitKind::Create { operator } => operator == expected,
                    WaitKind::Delete { name } => exact.iter().any(|r| &r.name == name),
                }
        });
        let has = |action| exact.iter().any(|r| r.normalized_action() == action);
        let state = match (has("allow"), has("deny")) {
            _ if pending => RuleState::Pending,
            (true, true) => RuleState::Partial,
            (true, false) => RuleState::Allow,
            (false, true) => RuleState::Deny,
            (false, false) if narrower => RuleState::Partial,
            (false, false) => RuleState::None,
        };
        Projection {
            state,
            names,
            effective: target.effective,
        }
    }
}

/// An enabled allow/deny rule of the user's own.
fn counts(rule: &Rule) -> bool {
    rule.enabled
        && matches!(rule.normalized_action(), "allow" | "deny")
        && rule.source() == RuleSource::User
}

/// Whether the (canonical) `operator` is narrower than a node at `level`
/// with group key `key`: for a program, any domain or host rule; for a
/// domain, a host rule inside it.
fn is_below(operator: &Value, level: Level, key: &str) -> bool {
    match level {
        Level::Program => classify(operator) != Level::Program,
        Level::Domain => {
            classify(operator) == Level::Host
                && dest_host(operator).is_some_and(|host| {
                    host == key.to_ascii_lowercase()
                        || host.ends_with(&format!(".{}", key.to_ascii_lowercase()))
                })
        }
        Level::Host => false,
    }
}

#[cfg(test)]
mod tests;
