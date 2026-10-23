//! The review page's state (learning mode, phase L4b), without Qt: which
//! program rows are offered, which choice the owner ticked on each, the
//! summary that repeats every rule, and the plan "Apply" sends.
//!
//! * NOTHING is ticked when a session is built. A row has at most ONE ticked
//!   choice, and only one the row offers (`tick`).
//! * "Select suggested" ticks the narrow "only these destinations" choice of
//!   the rows `ProgramProposal::selected_by_suggest` names, and only on rows
//!   with no tick yet: it never replaces the owner's own choice and never
//!   ticks "any destination", Block or a directory rule.
//! * `freeze` builds the plan once and keeps it with its plain-words lines
//!   (one per rule) and the extra confirmations Block of a system path or
//!   service and "any destination" for a dual-use program need. Any tick
//!   change drops it. `authorize` hands out the frozen plan only when every
//!   confirmation was given: a disabled button is not the check.
//! * Every method returns a new session; nothing is changed in place.

mod batch;
mod notices;
mod summary;
mod view;

pub use view::{kind_from_token, kind_token, progress_json};

use std::collections::HashMap;
use std::sync::Arc;

use crate::bulk_rules::{BulkPlan, PlanError};
use crate::learned::{Aggregate, Program};
use crate::learning_review::{
    build_review, programs_of, BuildRefusal, ChoiceKind, ProgramProposal, ReviewConfig,
};
use crate::rules::row_store::Rule;

pub use batch::{
    clear_refusal, delete_after, delete_record, read_rules, run_batch, run_batch_guarded,
    BatchOutcome,
};
pub use notices::Notices;
pub use summary::Summary;

/// Why a tick or an apply was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No such row, or the row does not offer that choice.
    NotOffered,
    /// Nothing is ticked.
    NothingTicked,
    /// The plan was not built (`freeze`) since the last change.
    NotFrozen,
    /// `n` of the extra confirmations were not given.
    Unconfirmed(usize),
    Build(BuildRefusal),
    Plan(PlanError),
}

impl Refusal {
    pub fn text(&self) -> String {
        match self {
            Self::NotOffered => "That choice is not offered for this program.".into(),
            Self::NothingTicked => "Nothing is ticked.".into(),
            Self::NotFrozen => "The summary has not been prepared.".into(),
            Self::Unconfirmed(n) => {
                format!("{n} confirmation(s) below the rules have not been ticked.")
            }
            Self::Build(_) => "A ticked choice could not be turned into a rule.".into(),
            Self::Plan(error) => error.text(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReviewSession {
    programs: Arc<HashMap<String, Program>>,
    proposals: Arc<Vec<ProgramProposal>>,
    ticks: HashMap<String, ChoiceKind>,
    notices: Notices,
    frozen: Option<summary::Frozen>,
}

impl ReviewSession {
    /// A session with nothing in it.
    pub fn empty() -> Self {
        Self::from_programs(Vec::new(), &[], Notices::default())
    }

    /// The review of `aggregate`; `rules` is the committed rules list, for
    /// the best-effort skip of a proposal a rule already makes.
    pub fn new(aggregate: &Aggregate, rules: &[Rule], rules_read: bool) -> Self {
        Self::from_programs(
            programs_of(aggregate),
            rules,
            Notices::of(aggregate, rules_read),
        )
    }

    fn from_programs(programs: Vec<Program>, rules: &[Rule], notices: Notices) -> Self {
        let proposals = build_review(&programs, rules, &ReviewConfig::default());
        Self {
            programs: Arc::new(programs.into_iter().map(|p| (p.key.clone(), p)).collect()),
            proposals: Arc::new(proposals),
            ticks: HashMap::new(),
            notices,
            frozen: None,
        }
    }

    pub fn proposals(&self) -> &[ProgramProposal] {
        &self.proposals
    }

    pub fn ticks(&self) -> &HashMap<String, ChoiceKind> {
        &self.ticks
    }

    pub fn tick_count(&self) -> usize {
        self.ticks.len()
    }

    pub fn notices(&self) -> &Notices {
        &self.notices
    }

    /// Tick `kind` on the row `key` (replacing its other tick), or untick it
    /// with `None`. A choice the row does not offer is refused.
    pub fn tick(&self, key: &str, kind: Option<ChoiceKind>) -> Result<Self, Refusal> {
        let mut ticks = self.ticks.clone();
        match kind {
            None => {
                ticks.remove(key);
            }
            Some(kind) => {
                let row = self
                    .proposals
                    .iter()
                    .find(|p| p.key == key)
                    .ok_or(Refusal::NotOffered)?;
                if kind == ChoiceKind::NonSelectable || row.choice(kind).is_none() {
                    return Err(Refusal::NotOffered);
                }
                ticks.insert(key.to_string(), kind);
            }
        }
        Ok(self.with_ticks(ticks))
    }

    /// The explicit "Select suggested" act.
    pub fn select_suggested(&self) -> Self {
        let mut ticks = self.ticks.clone();
        for row in self
            .proposals
            .iter()
            .filter(|p| p.suggested_choice().is_some())
        {
            ticks
                .entry(row.key.clone())
                .or_insert(ChoiceKind::AllowHosts);
        }
        self.with_ticks(ticks)
    }

    pub fn untick_all(&self) -> Self {
        self.with_ticks(HashMap::new())
    }

    fn with_ticks(&self, ticks: HashMap<String, ChoiceKind>) -> Self {
        Self {
            ticks,
            frozen: None,
            ..self.clone()
        }
    }

    /// Build the plan, its lines and its confirmations from the ticks.
    pub fn freeze(&self) -> Result<Self, Refusal> {
        let frozen = summary::freeze(self)?;
        Ok(Self {
            frozen: Some(frozen),
            ..self.clone()
        })
    }

    pub fn summary(&self) -> Option<Summary> {
        self.frozen.as_ref().map(summary::Frozen::summary)
    }

    /// Drop the frozen plan (the summary was closed).
    pub fn unfreeze(&self) -> Self {
        Self {
            frozen: None,
            ..self.clone()
        }
    }

    /// Give or take back extra confirmation `index`.
    pub fn confirm(&self, index: usize, on: bool) -> Self {
        Self {
            frozen: self.frozen.as_ref().map(|f| f.confirm(index, on)),
            ..self.clone()
        }
    }

    /// The frozen plan, if every extra confirmation was given.
    pub fn authorize(&self) -> Result<&BulkPlan, Refusal> {
        let frozen = self.frozen.as_ref().ok_or(Refusal::NotFrozen)?;
        match frozen.unconfirmed() {
            0 => Ok(frozen.plan()),
            n => Err(Refusal::Unconfirmed(n)),
        }
    }

    /// Up to `n` of a row's destinations with counts, busiest first.
    pub fn destinations(&self, key: &str, n: usize) -> Vec<(String, u64)> {
        self.proposals
            .iter()
            .find(|p| p.key == key)
            .map(|p| {
                p.top_destinations(n)
                    .into_iter()
                    .map(|(host, count)| (crate::app_names::sanitize_display(&host), count))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn program(&self, key: &str) -> Option<&Program> {
        self.programs.get(key)
    }
}

#[cfg(test)]
mod tests;
