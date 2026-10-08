//! The import preview: one [`ImportItem`] per rule in a document.
//!
//! [`check_rules`] does the expensive part (parsing and the policy, which
//! compiles regexps) without the cache; [`classify`] compares the result
//! with the cached rules, under the cache lock, cheaply.

use super::caution;
use super::{Document, PreviewError};
use crate::cache::rules::RulesCache;
use crate::rule_policy::{validate_user_rule, PolicyProfile, RuleProblem};
use crate::translator::verdict::strip_display_hazards;
use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::{Operator, Rule};
use std::collections::{BTreeMap, HashMap};

pub const DUPLICATE_NAME: &str = "the file has more than one rule with this name";
pub const ENABLED_MISSING: &str = "the rule doesn't say whether it is on or off";
pub const HIDDEN_NAME: &str = "the firewall already has a rule with this name that is too large \
     for Snitchwatch to show, so importing would replace it unseen";

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
    /// Not tied to particular programs: no condition at any depth that
    /// `rule_policy::binds_to_programs` counts (a real program file's path,
    /// or a command line).
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
    /// Whether the row starts ticked: an add or replace with no caution.
    pub ticked: bool,
    /// Why it starts unticked, in plain words.
    #[serde(default)]
    pub cautions: Vec<String>,
    /// The rule a replace overwrites, as it is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousRule>,
}

/// The cached rule a replace would overwrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousRule {
    pub enabled: bool,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub precedence: bool,
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
        // `rule_from_wire` reads a missing `enabled` as off and the GUI's
        // rule shape as on: an imported rule must say.
        Ok(_)
            if !value
                .get("enabled")
                .is_some_and(serde_json::Value::is_boolean) =>
        {
            Err(vec![RuleProblem {
                path: "enabled".into(),
                reason: ENABLED_MISSING.into(),
            }])
        }
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
) -> Result<ImportPreview, PreviewError> {
    let cached = cache.rules().ok_or(PreviewError::Unavailable)?;
    let left_out = cache.left_out();
    let mut applicable = BTreeMap::new();
    let items = checked
        .iter()
        .map(|rule| {
            let item = if left_out.contains_key(&rule.name) {
                let hidden = CheckedRule {
                    index: rule.index,
                    name: rule.name.clone(),
                    result: Err(vec![RuleProblem {
                        path: "name".into(),
                        reason: HIDDEN_NAME.into(),
                    }]),
                };
                classify_one(&hidden, None)
            } else {
                classify_one(rule, cached.get(&rule.name))
            };
            if matches!(item.kind, ImportKind::Add | ImportKind::Replace) {
                if let Ok(parsed) = &rule.result {
                    applicable.insert(parsed.name.clone(), parsed.clone());
                }
            }
            item
        })
        .collect();
    super::limits::check_totals(cached, left_out, &applicable)?;
    Ok(ImportPreview { items, applicable })
}

/// [`check_rules`] then [`classify`].
pub fn preview(document: &Document, cache: &RulesCache) -> Result<ImportPreview, PreviewError> {
    classify(&check_rules(document), cache)
}

fn classify_one(rule: &CheckedRule, cached: Option<&Rule>) -> ImportItem {
    let display_name = if rule.name.is_empty() {
        format!("Rule {} (its name can't be used)", rule.index + 1)
    } else {
        strip_display_hazards(&rule.name)
    };
    let item = ImportItem {
        index: rule.index,
        name: rule.name.clone(),
        display_name,
        ..empty_item()
    };
    match &rule.result {
        Err(problems) => ImportItem {
            kind: ImportKind::Refused,
            problems: problems.clone(),
            ..item
        },
        Ok(parsed) => classify_parsed(item, parsed, cached),
    }
}

/// An accepted rule's kind, flags, cautions and content.
fn classify_parsed(item: ImportItem, parsed: &Rule, cached: Option<&Rule>) -> ImportItem {
    let changed = cached.map(|old| changed_fields(old, parsed));
    let kind = match &changed {
        None => ImportKind::Add,
        Some(fields) if fields.is_empty() => ImportKind::Unchanged,
        Some(_) => ImportKind::Replace,
    };
    let changed = changed.unwrap_or_default();
    let all_apps = !parsed
        .operator
        .as_ref()
        .is_some_and(crate::rule_policy::binds_to_programs);
    let change = matches!(kind, ImportKind::Add | ImportKind::Replace);
    let cautions = if change {
        let same_conditions = !changed.contains(&"conditions");
        caution::cautions(cached, parsed, all_apps, same_conditions)
    } else {
        Vec::new()
    };
    ImportItem {
        kind,
        changed_fields: changed
            .iter()
            .map(|f| caution::plain_field(f).into())
            .collect(),
        weakens: kind == ImportKind::Replace && cached.is_some_and(|old| weakens(old, parsed)),
        applies_to_all_apps: all_apps,
        precedence: parsed.precedence,
        persists: parsed.duration == "always",
        enabled: parsed.enabled,
        action: parsed.action.clone(),
        duration: parsed.duration.clone(),
        description: strip_display_hazards(&parsed.description),
        nolog: parsed.nolog,
        conditions: parsed.operator.as_ref().map(conditions).unwrap_or_default(),
        ticked: change && cautions.is_empty(),
        cautions,
        previous: cached
            .filter(|_| kind == ImportKind::Replace)
            .map(|old| caution::previous(old, conditions)),
        ..item
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
        ticked: false,
        cautions: Vec::new(),
        previous: None,
    }
}

/// Whether `new` differs from `old` in nothing but `enabled` (a pure
/// toggle, P2.1): the list operand's spelling and `created` don't count.
pub fn only_enabled_differs(old: &Rule, new: &Rule) -> bool {
    changed_fields(old, new)
        .iter()
        .all(|field| *field == "enabled")
}

/// The plain-word cautions for writing `new` over `old` (`None`: a new
/// rule), as the import preview shows them; the rule editor shows the same.
pub fn edit_cautions(old: Option<&Rule>, new: &Rule) -> Vec<String> {
    let all_apps = !new
        .operator
        .as_ref()
        .is_some_and(crate::rule_policy::binds_to_programs);
    let same_conditions = old.is_none_or(|old| !changed_fields(old, new).contains(&"conditions"));
    caution::cautions(old, new, all_apps, same_conditions)
}

/// Whether two optional rules have the same content (see [`changed_fields`]):
/// how an apply checks a rule is still what the preview compared against.
pub fn same_rule(a: Option<&Rule>, b: Option<&Rule>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => changed_fields(a, b).is_empty(),
        _ => false,
    }
}

/// Fields that differ, ignoring `created` (the daemon stamps it) and the
/// spelling of a list operator (see [`normalised_operator`]).
fn changed_fields(old: &Rule, new: &Rule) -> Vec<&'static str> {
    let mut changed = Vec::new();
    let mut differs = |field: &'static str, same: bool| {
        if !same {
            changed.push(field);
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
    // Never shortened: the tail of a pattern can be what broadens it, and
    // the document cap already bounds the length.
    line.push_str(&shown);
    if leaf.sensitive {
        line.push_str(" (case-sensitive)");
    }
    if hidden {
        line.push_str(" (contains hidden characters, not shown)");
    }
    line
}
