//! The one "unedited" check for curated rules (code review M4).
//!
//! A rule the daemon reports back is not byte-for-byte what was sent.
//! opensnitchd v1.8.0 (`vendor:daemon/rule/rule.go`, `operator.go`,
//! `loader.go`):
//! - `Compile` sets a list operator's operand to `list`, and lowercases a
//!   case-insensitive regexp's data in place (only for an enabled rule);
//! - `Deserialize` and `Serialize` clear a list's own `data`, but a rule
//!   loaded from disk can still report its JSON there;
//! - `created` is the daemon's, and `enabled` is the user's to toggle.
//!
//! [`canonical`] applies those, so [`is_unedited`] compares meaning, not
//! bytes. The canonical form is also what the choices file records of an
//! installed copy, so no display or policy field leaks into it.

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::{Operator, Rule};

use super::CuratedEntry;

/// A condition, as the daemon applies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalLeaf {
    #[serde(rename = "type")]
    pub kind: String,
    pub operand: String,
    pub data: String,
    pub sensitive: bool,
    /// A list's members, in the daemon's order (it keeps the order sent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<CanonicalLeaf>,
}

/// A rule's meaning: everything but `enabled` and `created`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalRule {
    pub name: String,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub precedence: bool,
    pub nolog: bool,
    pub operator: Option<CanonicalLeaf>,
}

/// `rule` as opensnitchd applies it.
pub fn canonical(rule: &Rule) -> CanonicalRule {
    CanonicalRule {
        name: rule.name.clone(),
        action: rule.action.clone(),
        duration: rule.duration.clone(),
        description: rule.description.clone(),
        precedence: rule.precedence,
        nolog: rule.nolog,
        operator: rule.operator.as_ref().map(canonical_leaf),
    }
}

fn canonical_leaf(op: &Operator) -> CanonicalLeaf {
    if op.r#type == "list" {
        return CanonicalLeaf {
            kind: "list".into(),
            operand: "list".into(),
            data: String::new(),
            sensitive: false,
            list: op.list.iter().map(canonical_leaf).collect(),
        };
    }
    let data = if op.r#type == "regexp" && !op.sensitive {
        op.data.to_lowercase()
    } else {
        op.data.clone()
    };
    CanonicalLeaf {
        kind: op.r#type.clone(),
        operand: op.operand.clone(),
        data,
        sensitive: op.sensitive,
        list: op.list.iter().map(canonical_leaf).collect(),
    }
}

impl CanonicalRule {
    /// The rule this form describes, enabled, with no `created`.
    pub fn to_rule(&self) -> Rule {
        Rule {
            created: 0,
            name: self.name.clone(),
            description: self.description.clone(),
            enabled: true,
            precedence: self.precedence,
            nolog: self.nolog,
            action: self.action.clone(),
            duration: self.duration.clone(),
            operator: self.operator.as_ref().map(CanonicalLeaf::to_operator),
        }
    }
}

impl CanonicalLeaf {
    fn to_operator(&self) -> Operator {
        Operator {
            r#type: self.kind.clone(),
            operand: self.operand.clone(),
            data: self.data.clone(),
            sensitive: self.sensitive,
            list: self.list.iter().map(Self::to_operator).collect(),
        }
    }
}

/// Whether the daemon's `daemon_rule` is unedited: the data file's rule for
/// `entry` (or, for an entry no longer in the file, the `recorded` copy
/// Snitchwatch installed), apart from what [`canonical`] ignores. The one
/// check behind reconcile, the Rules page's toggle and the send path.
pub fn is_unedited(
    entry: Option<&CuratedEntry>,
    recorded: Option<&CanonicalRule>,
    daemon_rule: &Rule,
) -> bool {
    let theirs = canonical(daemon_rule);
    match (entry, recorded) {
        (Some(entry), _) => theirs == canonical(&entry.rule()),
        (None, Some(recorded)) => &theirs == recorded,
        (None, None) => false,
    }
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod tests;
