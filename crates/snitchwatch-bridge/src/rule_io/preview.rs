//! The import preview: one [`ImportItem`] per rule in a document.
//!
//! [`check_rules`] does the expensive part (parsing and the policy, which
//! compiles regexps) without the cache; [`classify`] compares the result
//! with the cached rules, under the cache lock, cheaply.

use super::{Document, PreviewUnavailable};
use crate::cache::rules::RulesCache;
use crate::rule_policy::{validate_user_rule, PolicyProfile, RuleProblem};
use crate::translator::verdict::strip_display_hazards;
use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::{Operator, Rule};
use std::collections::{BTreeMap, HashMap};

pub const DUPLICATE_NAME: &str = "the file has more than one rule with this name";
/// Longest condition value shown in full.
const MAX_SHOWN_DATA_CHARS: usize = 200;

/// What applying a previewed rule would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportKind {
    /// No rule by this name: a new rule.
    Add,
    /// Same name, different content: the daemon overwrites it in place.
    Replace,
    /// Same name and content: nothing to do.
    Unchanged,
    /// Not installable; see `problems`.
    Refused,
}

/// One rule of the file, as the preview shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportItem {
    /// Position in the file.
    pub index: u32,
    /// The exact name, which `ApplyRulesImport` names; empty when the name
    /// is missing or unusable.
    pub name: String,
    pub display_name: String,
    pub kind: ImportKind,
    #[serde(default)]
    pub changed_fields: Vec<String>,
    #[serde(default)]
    pub problems: Vec<RuleProblem>,
    /// A replace that loosens: deny/reject to allow, a deny/reject
    /// disabled, or `precedence` added to an allow.
    pub weakens: bool,
    /// No `process.*` condition at any depth.
    pub applies_to_all_apps: bool,
    pub precedence: bool,
    /// The rule is saved to disk (`always`).
    pub persists: bool,
    // What would be installed; empty for a refused rule.
    pub enabled: bool,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub nolog: bool,
    pub conditions: Vec<String>,
}

/// Every rule's item, and the rules an apply may send by name.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPreview {
    pub items: Vec<ImportItem>,
    /// The `Add` and `Replace` rules, by name.
    pub applicable: BTreeMap<String, Rule>,
}

/// A file rule after parsing and the policy.
#[derive(Debug, Clone)]
pub struct CheckedRule {
    index: u32,
    name: String,
    result: Result<Rule, Vec<RuleProblem>>,
}

/// Parse and check every rule. No cache needed; this is the slow part.
pub fn check_rules(document: &Document) -> Vec<CheckedRule> {
    let mut checked: Vec<CheckedRule> = document
        .rules
        .iter()
        .enumerate()
        .map(|(index, value)| check_rule(index as u32, value))
        .collect();
    let mut seen: HashMap<String, u32> = HashMap::new();
    for rule in &checked {
        if !rule.name.is_empty() {
            *seen.entry(rule.name.clone()).or_default() += 1;
        }
    }
    for rule in &mut checked {
        if seen.get(&rule.name).is_some_and(|count| *count > 1) {
            let duplicate = RuleProblem {
                path: "name".into(),
                reason: DUPLICATE_NAME.into(),
            };
            match &mut rule.result {
                Ok(_) => rule.result = Err(vec![duplicate]),
                Err(problems) => problems.push(duplicate),
            }
        }
    }
    checked
}

fn check_rule(index: u32, value: &serde_json::Value) -> CheckedRule {
    let raw_name = value.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let name = if crate::rule_name::validate_rule_name(raw_name).is_ok() {
        raw_name.to_string()
    } else {
        String::new()
    };
    let result = match crate::rule_wire::rule_from_wire(value) {
        Err(reason) => Err(vec![RuleProblem {
            path: "rule".into(),
            reason,
        }]),
        Ok(rule) => validate_user_rule(&rule, PolicyProfile::Import).map(|()| rule),
    };
    CheckedRule {
        index,
        name,
        result,
    }
}

/// Compare checked rules with the cache. Refused while it is `Unknown`.
pub fn classify(
    checked: &[CheckedRule],
    cache: &RulesCache,
) -> Result<ImportPreview, PreviewUnavailable> {
    let cached = cache.rules().ok_or(PreviewUnavailable)?;
    let mut applicable = BTreeMap::new();
    let items = checked
        .iter()
        .map(|rule| {
            let item = classify_one(rule, cached.get(&rule.name));
            if matches!(item.kind, ImportKind::Add | ImportKind::Replace) {
                if let Ok(parsed) = &rule.result {
                    applicable.insert(parsed.name.clone(), parsed.clone());
                }
            }
            item
        })
        .collect();
    Ok(ImportPreview { items, applicable })
}

/// [`check_rules`] then [`classify`].
pub fn preview(
    document: &Document,
    cache: &RulesCache,
) -> Result<ImportPreview, PreviewUnavailable> {
    classify(&check_rules(document), cache)
}

fn classify_one(rule: &CheckedRule, cached: Option<&Rule>) -> ImportItem {
    let display_name = if rule.name.is_empty() {
        format!("Rule {} (its name can't be used)", rule.index + 1)
    } else {
        strip_display_hazards(&rule.name)
    };
    let parsed = match &rule.result {
        Err(problems) => {
            return ImportItem {
                index: rule.index,
                name: rule.name.clone(),
                display_name,
                kind: ImportKind::Refused,
                problems: problems.clone(),
                ..empty_item()
            }
        }
        Ok(parsed) => parsed,
    };
    let changed_fields = cached.map(|old| changed_fields(old, parsed));
    let kind = match &changed_fields {
        None => ImportKind::Add,
        Some(fields) if fields.is_empty() => ImportKind::Unchanged,
        Some(_) => ImportKind::Replace,
    };
    ImportItem {
        index: rule.index,
        name: rule.name.clone(),
        display_name,
        kind,
        changed_fields: changed_fields.unwrap_or_default(),
        problems: Vec::new(),
        weakens: kind == ImportKind::Replace && cached.is_some_and(|old| weakens(old, parsed)),
        applies_to_all_apps: !parsed.operator.as_ref().is_some_and(has_process_operand),
        precedence: parsed.precedence,
        persists: parsed.duration == "always",
        enabled: parsed.enabled,
        action: parsed.action.clone(),
        duration: parsed.duration.clone(),
        description: strip_display_hazards(&parsed.description),
        nolog: parsed.nolog,
        conditions: parsed.operator.as_ref().map(conditions).unwrap_or_default(),
    }
}

fn empty_item() -> ImportItem {
    ImportItem {
        index: 0,
        name: String::new(),
        display_name: String::new(),
        kind: ImportKind::Refused,
        changed_fields: Vec::new(),
        problems: Vec::new(),
        weakens: false,
        applies_to_all_apps: false,
        precedence: false,
        persists: false,
        enabled: false,
        action: String::new(),
        duration: String::new(),
        description: String::new(),
        nolog: false,
        conditions: Vec::new(),
    }
}

/// Fields that differ, ignoring `created` (the daemon stamps it) and the
/// spelling of a list operator (see [`normalised_operator`]).
fn changed_fields(old: &Rule, new: &Rule) -> Vec<String> {
    let mut changed = Vec::new();
    let mut differs = |field: &str, same: bool| {
        if !same {
            changed.push(field.to_string());
        }
    };
    differs("enabled", old.enabled == new.enabled);
    differs("action", old.action == new.action);
    differs("duration", old.duration == new.duration);
    differs("description", old.description == new.description);
    differs("precedence", old.precedence == new.precedence);
    differs("nolog", old.nolog == new.nolog);
    differs(
        "conditions",
        old.operator.as_ref().map(normalised_operator)
            == new.operator.as_ref().map(normalised_operator),
    );
    changed
}

/// A list operator's own operand, data and sensitivity are unused by the
/// daemon (`listMatch` reads only the members). `Compile` sets the operand
/// to `list`, `operator_from_wire` leaves it empty, and a stock-UI rule
/// loaded from disk can keep its JSON in `data` (`Rule.Serialize` copies it
/// before clearing it), so all three are normalised away.
fn normalised_operator(op: &Operator) -> Operator {
    let mut op = op.clone();
    if op.r#type == "list" {
        op.operand = "list".into();
        op.data.clear();
        op.sensitive = false;
    }
    op.list = op.list.iter().map(normalised_operator).collect();
    op
}

fn is_deny(rule: &Rule) -> bool {
    matches!(rule.action.as_str(), "deny" | "reject")
}

fn weakens(old: &Rule, new: &Rule) -> bool {
    let deny_to_allow = is_deny(old) && new.action == "allow";
    let deny_disabled = is_deny(old) && old.enabled && !new.enabled;
    let precedence_added =
        new.action == "allow" && new.precedence && !(old.action == "allow" && old.precedence);
    deny_to_allow || deny_disabled || precedence_added
}

fn has_process_operand(op: &Operator) -> bool {
    op.operand.starts_with("process.") || op.list.iter().any(has_process_operand)
}

/// One plain-text line per leaf condition.
fn conditions(op: &Operator) -> Vec<String> {
    if op.r#type == "list" {
        op.list.iter().map(condition).collect()
    } else {
        vec![condition(op)]
    }
}

fn condition(leaf: &Operator) -> String {
    if leaf.operand == "true" {
        return "always".to_string();
    }
    let verb = match leaf.r#type.as_str() {
        "regexp" => "matches",
        "network" => "is in",
        _ => "is",
    };
    let shown = strip_display_hazards(&leaf.data);
    let hidden = shown.chars().count() != leaf.data.chars().count();
    let mut line = format!("{} {verb} ", leaf.operand);
    line.extend(shown.chars().take(MAX_SHOWN_DATA_CHARS));
    if shown.chars().count() > MAX_SHOWN_DATA_CHARS {
        line.push('…');
    }
    if leaf.sensitive {
        line.push_str(" (case-sensitive)");
    }
    if hidden {
        line.push_str(" (contains hidden characters, not shown)");
    }
    line
}
