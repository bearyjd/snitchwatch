//! The rules a batch will send, checked before anything is sent.

use snitchwatch_bridge::rule_name::{is_reserved_name, validate_rule_name};

use crate::learning_review::RulePayload;

/// The prompt rules' legacy prefix. Not enforced by the bridge, but a
/// learned rule must never take a name in that family.
pub const LEGACY_ALLOW_PREFIX: &str = "snitchwatch-allow-";

/// One rule to send and the words the list shows for it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRule {
    pub payload: RulePayload,
    pub label: String,
}

impl PlannedRule {
    pub fn name(&self) -> &str {
        self.payload.name()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Empty,
    /// The name is in a family Snitchwatch keeps for itself.
    ReservedName(String),
    /// The bridge would refuse the name.
    InvalidName(String),
}

impl PlanError {
    pub fn text(&self) -> String {
        match self {
            Self::Empty => "There are no rules to apply.".into(),
            Self::ReservedName(name) => {
                format!("The rule name {name} is in a family Snitchwatch keeps for itself.")
            }
            Self::InvalidName(name) => format!("The rule name {name} cannot be sent."),
        }
    }
}

/// A checked, de-duplicated list of rules, in the order they will be sent.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkPlan {
    rules: Vec<PlannedRule>,
    dropped: Vec<String>,
}

impl BulkPlan {
    /// Refuses an empty list and any reserved or unsendable name. A second
    /// rule with a name already in the list is dropped (the first is kept)
    /// and reported by [`Self::dropped`].
    pub fn new(rules: Vec<PlannedRule>) -> Result<Self, PlanError> {
        let mut kept: Vec<PlannedRule> = Vec::with_capacity(rules.len());
        let mut dropped = Vec::new();
        for rule in rules {
            let name = rule.name();
            if is_reserved_name(name) || name.starts_with(LEGACY_ALLOW_PREFIX) {
                return Err(PlanError::ReservedName(name.to_owned()));
            }
            if validate_rule_name(name).is_err() {
                return Err(PlanError::InvalidName(name.to_owned()));
            }
            if kept.iter().all(|k| k.name() != name) {
                kept.push(rule);
            } else {
                dropped.push(name.to_owned());
            }
        }
        if kept.is_empty() {
            return Err(PlanError::Empty);
        }
        Ok(Self {
            rules: kept,
            dropped,
        })
    }

    pub fn rules(&self) -> &[PlannedRule] {
        &self.rules
    }

    /// Names that came more than once and were planned once.
    pub fn dropped(&self) -> &[String] {
        &self.dropped
    }
}
