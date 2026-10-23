//! The frozen plan: what "Apply" will send, said rule by rule in plain words,
//! and the extra confirmations that go with it.

use super::{Refusal, ReviewSession};
use crate::bulk_rules::{BulkPlan, PlannedRule};
use crate::learning_review::{rules_for, Choice, ProgramProposal};

/// What the summary dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// One sentence per rule that will be sent, in sending order.
    pub lines: Vec<String>,
    /// Confirmations, each a sentence; every one must be given.
    pub extras: Vec<String>,
    pub confirmed: Vec<bool>,
    /// Repeated rule names that were planned once.
    pub dropped_names: usize,
}

#[derive(Debug, Clone)]
pub(super) struct Frozen {
    plan: BulkPlan,
    extras: Vec<String>,
    confirmed: Vec<bool>,
}

impl Frozen {
    pub(super) fn plan(&self) -> &BulkPlan {
        &self.plan
    }

    pub(super) fn unconfirmed(&self) -> usize {
        self.confirmed.iter().filter(|given| !**given).count()
    }

    pub(super) fn confirm(&self, index: usize, on: bool) -> Self {
        let confirmed = self
            .confirmed
            .iter()
            .enumerate()
            .map(|(i, given)| if i == index { on } else { *given })
            .collect();
        Self {
            confirmed,
            ..self.clone()
        }
    }

    pub(super) fn summary(&self) -> Summary {
        Summary {
            lines: self
                .plan
                .rules()
                .iter()
                .map(|rule| rule.label.clone())
                .collect(),
            extras: self.extras.clone(),
            confirmed: self.confirmed.clone(),
            dropped_names: self.plan.dropped().len(),
        }
    }
}

/// `name ? (path)`: the label with its marker and the sanitised real path.
fn who(row: &ProgramProposal) -> String {
    let mark = if row.marker { " ?" } else { "" };
    format!("{}{} ({})", row.name.label, mark, row.display)
}

fn sentences(row: &ProgramProposal, choice: &Choice, count: usize) -> Vec<String> {
    let who = who(row);
    match choice {
        Choice::AllowAny => vec![format!("Allow {who} to connect to ANY destination.")],
        Choice::AllowHosts(hosts) => hosts
            .iter()
            .take(count)
            .map(|host| format!("Allow {who} to connect to {host}, on any port."))
            .collect(),
        Choice::Block => vec![format!(
            "BLOCK {who}: every connection it makes will be denied."
        )],
        Choice::DirectoryRule(rule) => vec![format!(
            "Allow ANY program file under this folder to connect to ANY destination. {}",
            rule.text()
        )],
        Choice::NonSelectable(_) => Vec::new(),
    }
}

fn extra_for(row: &ProgramProposal, choice: &Choice) -> Option<String> {
    let warning = match choice {
        Choice::Block => row.block_warning.as_deref(),
        Choice::AllowAny => row.allow_any_warning.as_deref(),
        _ => None,
    }?;
    Some(format!("{} {} I understand.", who(row), warning))
}

pub(super) fn freeze(session: &ReviewSession) -> Result<Frozen, Refusal> {
    if session.ticks.is_empty() {
        return Err(Refusal::NothingTicked);
    }
    let mut planned = Vec::new();
    let mut extras = Vec::new();
    for row in session.proposals.iter() {
        let Some(kind) = session.ticks.get(&row.key) else {
            continue;
        };
        let (Some(choice), Some(program)) = (row.choice(*kind), session.program(&row.key)) else {
            return Err(Refusal::NotOffered);
        };
        let payloads = rules_for(choice, program).map_err(Refusal::Build)?;
        let lines = sentences(row, choice, payloads.len());
        planned.extend(
            payloads
                .into_iter()
                .zip(lines)
                .map(|(payload, label)| PlannedRule { payload, label }),
        );
        extras.extend(extra_for(row, choice));
    }
    let plan = BulkPlan::new(planned).map_err(Refusal::Plan)?;
    Ok(Frozen {
        confirmed: vec![false; extras.len()],
        extras,
        plan,
    })
}
