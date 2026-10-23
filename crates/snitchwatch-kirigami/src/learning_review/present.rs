//! A proposal an existing rule already makes (best effort).
//!
//! The comparison is over CONTENT, not names: an enabled rule with the same
//! action, duration "always" and the same operator tree. The tree is
//! normalised the way the daemon reads it: `dest.host` data compared without
//! case, `AND` lists in any order, a one-member list as its member, nested
//! lists flattened. The name, description and precedence are ignored.
//! Anything that cannot be read is simply not a match, so the proposal
//! stays on offer.

use serde_json::Value;

use super::rule::RulePayload;
use crate::rules::row_store::Rule;

/// Deepest list nesting read (the bridge refuses deeper ones anyway).
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Node {
    Leaf {
        kind: String,
        operand: String,
        data: String,
        sensitive: bool,
    },
    All(Vec<Node>),
}

fn node(value: &Value, depth: usize) -> Option<Node> {
    if depth > MAX_DEPTH {
        return None;
    }
    let kind = value.get("type")?.as_str()?.to_ascii_lowercase();
    let members = value
        .get("operands")
        .or_else(|| value.get("list"))
        .and_then(Value::as_array)
        .filter(|members| !members.is_empty());
    if kind == "list" || members.is_some() {
        let mut flat = Vec::new();
        for member in members? {
            match node(member, depth + 1)? {
                Node::All(inner) => flat.extend(inner),
                leaf => flat.push(leaf),
            }
        }
        flat.sort();
        return Some(match flat.len() {
            1 => flat.remove(0),
            _ => Node::All(flat),
        });
    }
    let operand = value.get("operand")?.as_str()?.to_ascii_lowercase();
    let data = value.get("data")?.as_str()?;
    Some(Node::Leaf {
        data: if operand == "dest.host" {
            data.to_ascii_lowercase()
        } else {
            data.to_string()
        },
        kind,
        operand,
        sensitive: value
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Whether an enabled rule in `existing` makes what `payload` makes.
pub(super) fn is_present(payload: &RulePayload, existing: &[Rule]) -> bool {
    let wire = payload.to_wire();
    let Some(wanted) = wire.get("operator").and_then(|op| node(op, 0)) else {
        return false;
    };
    existing.iter().any(|rule| {
        rule.enabled
            && rule.normalized_action() == payload.rule.action
            && rule.duration == payload.rule.duration
            && node(&rule.operator, 0).as_ref() == Some(&wanted)
    })
}
