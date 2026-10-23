//! A scripted bridge: records what the sequencer does, answers from a
//! script, and keeps its own clock so no test waits.

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::Value;
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};
use snitchwatch_proto::protocol::{Operator, Rule};

use crate::bulk_rules::{
    run, BulkPlan, Incoming, PlannedRule, Progress, RuleSink, SendError, SessionLost, Settings,
};
use crate::learning_review::RulePayload;

pub fn payload_with(name: &str, path: &str) -> RulePayload {
    RulePayload {
        rule: Rule {
            name: name.into(),
            enabled: true,
            action: "allow".into(),
            duration: "always".into(),
            operator: Some(Operator {
                r#type: "simple".into(),
                operand: "process.path".into(),
                data: path.into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    }
}

pub fn planned(name: &str) -> PlannedRule {
    PlannedRule {
        payload: payload_with(name, &format!("/usr/bin/{name}")),
        label: format!("label {name}"),
    }
}

pub fn plan_of(names: &[&str]) -> BulkPlan {
    BulkPlan::new(names.iter().map(|n| planned(n)).collect()).expect("plan")
}

/// What the bridge does with one `AddRule`.
#[derive(Clone)]
pub enum Answer {
    /// Apply it and answer with this outcome.
    Applied(RuleCommandOutcome),
    /// Answer with this outcome and do not apply it.
    Not(RuleCommandOutcome),
    /// Answer `Timeout`, having applied it.
    TimeoutApplied,
    /// Answer `Timeout`; a rule of the same name but other content appears.
    TimeoutOther,
    /// Never answer, and do not apply it.
    Silent,
    /// The feed lags and the answer is lost; `applied` says whether the
    /// daemon took the rule.
    Lagged { applied: bool },
}

pub struct Fake {
    /// The daemon's rules, as wire values.
    pub rules: Vec<Value>,
    pub listed: bool,
    pub too_large: u32,
    pub over_limit_total: Option<u32>,
    pub answers_snapshot: bool,
    /// One per `AddRule`; `Applied(Ok)` once it runs out.
    pub answers: VecDeque<Answer>,
    pub send_errors: VecDeque<SendError>,
    /// `request_snapshot` fails with this.
    pub snapshot_error: Option<SendError>,
    pub cancel_after_adds: Option<usize>,
    pub lose_session_on_next_wait: bool,
    /// Sent before the real answer: results for other requests.
    pub stray_results: bool,
    /// After the first `AddRule`, the bridge stops answering snapshots.
    pub snapshots_go_dark_after_add: bool,
    /// After this many adds the bridge withdraws the list (an empty
    /// `SetRules`, then `listed: false`) and stays that way.
    pub withdraw_after_adds: Option<usize>,
    /// The bridge's real answer to a rule command: the list again, then
    /// `RulesNotShown { listed: true }`.
    pub list_after_add: bool,
    /// The bridge withdraws the list while the sequencer sleeps.
    pub withdraw_on_sleep: bool,
    /// Cancel is pressed once this many snapshots have been asked for.
    pub cancel_after_snapshots: Option<usize>,
    /// Cancel is pressed once this many zero-wait reads (drains) were made.
    pub cancel_after_drains: Option<usize>,
    drains: usize,
    pub left_on_disk: u32,
    pub count_mismatch: bool,
    pub log: Vec<String>,
    pub sleeps: Vec<Duration>,
    pub clock: Duration,
    inbox: VecDeque<Incoming>,
    adds: usize,
    next_id: u32,
}

impl Fake {
    pub fn new() -> Self {
        Self {
            rules: vec![],
            listed: true,
            too_large: 0,
            over_limit_total: None,
            answers_snapshot: true,
            answers: VecDeque::new(),
            send_errors: VecDeque::new(),
            snapshot_error: None,
            cancel_after_adds: None,
            lose_session_on_next_wait: false,
            stray_results: false,
            snapshots_go_dark_after_add: false,
            withdraw_after_adds: None,
            list_after_add: false,
            withdraw_on_sleep: false,
            cancel_after_snapshots: None,
            cancel_after_drains: None,
            drains: 0,
            left_on_disk: 0,
            count_mismatch: false,
            log: vec![],
            sleeps: vec![],
            clock: Duration::ZERO,
            inbox: VecDeque::new(),
            adds: 0,
            next_id: 0,
        }
    }

    pub fn with_answers(answers: &[Answer]) -> Self {
        Self {
            answers: answers.iter().cloned().collect(),
            ..Self::new()
        }
    }

    /// The names sent in an `AddRule`, in order.
    pub fn added(&self) -> Vec<String> {
        self.log
            .iter()
            .filter_map(|l| l.strip_prefix("add:").map(str::to_owned))
            .collect()
    }

    pub fn snapshots(&self) -> usize {
        self.log.iter().filter(|l| *l == "snapshot").count()
    }

    fn list_messages(&self) -> Vec<ServerMessage> {
        vec![
            ServerMessage::SetRules {
                rules: if self.listed {
                    self.rules.clone()
                } else {
                    vec![]
                },
            },
            ServerMessage::RulesNotShown {
                too_large: self.too_large,
                over_limit_total: self.over_limit_total,
                listed: self.listed,
                left_on_disk: self.left_on_disk,
                count_mismatch: self.count_mismatch,
            },
        ]
    }
}

impl RuleSink for Fake {
    fn new_request_id(&mut self) -> String {
        self.next_id += 1;
        format!("t-{}", self.next_id)
    }

    fn send_add(&mut self, request_id: &str, rule: &RulePayload) -> Result<(), SendError> {
        if let Some(error) = self.send_errors.pop_front() {
            self.log.push(format!("full:{}", rule.name()));
            return Err(error);
        }
        self.log.push(format!("add:{}", rule.name()));
        self.adds += 1;
        if self.stray_results {
            self.inbox
                .push_back(Incoming::Message(ServerMessage::RuleCommandResult {
                    request_id: "someone-else".into(),
                    outcome: RuleCommandOutcome::Rejected {
                        reason: "stray".into(),
                    },
                }));
        }
        let answer = self
            .answers
            .pop_front()
            .unwrap_or(Answer::Applied(RuleCommandOutcome::Ok));
        if self.snapshots_go_dark_after_add {
            self.answers_snapshot = false;
        }
        if matches!(answer, Answer::TimeoutOther) {
            self.rules
                .push(payload_with(rule.name(), "/elsewhere").to_wire());
        }
        let (applies, outcome) = match answer {
            Answer::Applied(o) => (true, Some(o)),
            Answer::Not(o) => (false, Some(o)),
            Answer::TimeoutApplied => (true, Some(RuleCommandOutcome::Timeout)),
            Answer::TimeoutOther => (false, Some(RuleCommandOutcome::Timeout)),
            Answer::Silent => (false, None),
            Answer::Lagged { applied } => {
                self.inbox.push_back(Incoming::Lagged);
                (applied, None)
            }
        };
        if applies {
            self.rules.push(rule.to_wire());
        }
        if let Some(outcome) = outcome {
            self.inbox
                .push_back(Incoming::Message(ServerMessage::RuleCommandResult {
                    request_id: request_id.to_owned(),
                    outcome,
                }));
        }
        if self.list_after_add {
            self.inbox
                .extend(self.list_messages().into_iter().map(Incoming::Message));
        }
        if self.withdraw_after_adds == Some(self.adds) {
            self.listed = false;
            self.inbox
                .extend(self.list_messages().into_iter().map(Incoming::Message));
        }
        Ok(())
    }

    fn request_snapshot(&mut self) -> Result<(), SendError> {
        if let Some(error) = self.snapshot_error {
            return Err(error);
        }
        self.log.push("snapshot".into());
        if self.answers_snapshot {
            self.inbox
                .extend(self.list_messages().into_iter().map(Incoming::Message));
        }
        Ok(())
    }

    fn next_message(&mut self, wait: Duration) -> Result<Incoming, SessionLost> {
        if wait.is_zero() {
            self.drains += 1;
        }
        if self.lose_session_on_next_wait {
            return Err(SessionLost);
        }
        match self.inbox.pop_front() {
            Some(incoming) => Ok(incoming),
            None => {
                self.clock += wait;
                Ok(Incoming::Idle)
            }
        }
    }

    fn sleep(&mut self, duration: Duration) {
        self.sleeps.push(duration);
        if self.withdraw_on_sleep {
            self.listed = false;
            self.inbox
                .extend(self.list_messages().into_iter().map(Incoming::Message));
        }
        self.clock += duration;
    }

    fn now(&self) -> Duration {
        self.clock
    }

    fn cancelled(&self) -> bool {
        self.cancel_after_adds.is_some_and(|n| self.adds >= n)
            || self.cancel_after_drains.is_some_and(|n| self.drains >= n)
            || self
                .cancel_after_snapshots
                .is_some_and(|n| self.snapshots() >= n)
    }
}

pub fn run_plan(plan: &BulkPlan, fake: &mut Fake) -> Progress {
    run(plan, fake, &Settings::default(), &mut |_| {})
}
