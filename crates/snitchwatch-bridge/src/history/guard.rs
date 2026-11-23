//! One accepted AskRule owns its arrival pin until its terminal outcome.

use std::sync::Arc;

use snitchwatch_proto::protocol::Rule;

use super::capture::{map_rule, Mapped, Shared};
use super::pins::PinId;
use super::record::{Action, AnsweredBy, EventRecord, PromptOutcome};
use super::Consent;

pub enum DecisionOutcome<'a> {
    Replied {
        rule: &'a Rule,
        answered_by: AnsweredBy,
    },
    Timeout {
        action: Option<&'a str>,
    },
    Error {
        action: Option<&'a str>,
    },
    GuiLost,
    Cancelled,
}

impl DecisionOutcome<'_> {
    pub(super) fn map(self, mut record: EventRecord, now: i64) -> Mapped {
        record.decided_ts_ms = Some(now);
        record.prompt_outcome = Some(match self {
            Self::Replied { rule, answered_by } => {
                record.answered_by = answered_by;
                record.prompt_outcome = Some(PromptOutcome::Replied);
                return map_rule(record, rule);
            }
            Self::Timeout { action } => {
                record.answered_by = AnsweredBy::NoAnswer;
                record.action = default_action(action);
                PromptOutcome::Timeout
            }
            Self::Error { action } => {
                record.action = default_action(action);
                PromptOutcome::Error
            }
            Self::GuiLost => PromptOutcome::GuiLost,
            Self::Cancelled => PromptOutcome::Cancelled,
        });
        Mapped {
            record,
            identity: None,
            nolog: false,
        }
    }
}

fn default_action(action: Option<&str>) -> Action {
    match action {
        Some("allow") => Action::DefaultAllow,
        Some("deny") => Action::DefaultDeny,
        _ => Action::PendingUnanswered,
    }
}

pub struct DecisionGuard {
    shared: Arc<Shared>,
    consent: Arc<Consent>,
    generation: u64,
    pending: Option<(EventRecord, PinId)>,
}

impl DecisionGuard {
    pub(super) fn new(
        shared: Arc<Shared>,
        record: EventRecord,
        consent: Arc<Consent>,
        generation: u64,
        pin: PinId,
    ) -> Self {
        Self {
            shared,
            consent,
            generation,
            pending: Some((record, pin)),
        }
    }

    pub fn settle(mut self, outcome: DecisionOutcome<'_>) {
        self.finish(outcome);
    }

    fn finish(&mut self, outcome: DecisionOutcome<'_>) {
        if let Some((record, pin)) = self.pending.take() {
            self.shared
                .settle(record, &self.consent, self.generation, pin, outcome);
        }
    }
}

impl Drop for DecisionGuard {
    fn drop(&mut self) {
        self.finish(DecisionOutcome::Cancelled);
    }
}
