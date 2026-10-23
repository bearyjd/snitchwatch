//! Whether a rule already in the list has the content of a planned one.
//!
//! The comparison is over what decides traffic: action, duration, enabled,
//! precedence, nolog and the operator tree. The description, creation time
//! and display fields are not content. The tree is normalised (kind case,
//! operand whitespace, a missing `sensitive` as false, an empty `operands`
//! as a leaf, the members of an AND list in any order) and anything that
//! cannot be read is NEVER equal: a false "same" would hide an edit.

use serde_json::Value;

use super::plan::PlannedRule;
use crate::rules::row_store::Rule;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Leaf {
        kind: String,
        operand: String,
        data: String,
        sensitive: bool,
    },
    All {
        kind: String,
        items: Vec<Node>,
    },
}

fn text(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    match object.get(key) {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

fn node(value: &Value) -> Option<Node> {
    let object = value.as_object()?;
    let kind = text(object, "type")?.trim().to_lowercase();
    match object.get("operands") {
        Some(Value::Array(items)) if !items.is_empty() => {
            let mut items = items.iter().map(node).collect::<Option<Vec<_>>>()?;
            items.sort();
            Some(Node::All { kind, items })
        }
        None | Some(Value::Null) | Some(Value::Array(_)) => {
            let sensitive = match object.get("sensitive") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return None,
            };
            Some(Node::Leaf {
                kind,
                operand: text(object, "operand")?.trim().to_owned(),
                data: text(object, "data")?,
                sensitive,
            })
        }
        Some(_) => None,
    }
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// True only when `existing` has the content `planned` would give it.
pub fn same_content(planned: &PlannedRule, existing: &Rule) -> bool {
    let wire = planned.payload.to_wire();
    let Some(wanted) = wire.get("operator").and_then(node) else {
        return false;
    };
    let Some(found) = node(&existing.operator) else {
        return false;
    };
    let flag = |key: &str| wire.get(key).and_then(Value::as_bool);
    let word = |key: &str| wire.get(key).and_then(Value::as_str).unwrap_or_default();
    wanted == found
        && same_text(word("action"), &existing.action)
        && existing.duration.trim() == word("duration").trim()
        && flag("enabled") == Some(existing.enabled)
        && flag("precedence") == Some(existing.precedence)
        && flag("nolog") == Some(existing.nolog)
}
