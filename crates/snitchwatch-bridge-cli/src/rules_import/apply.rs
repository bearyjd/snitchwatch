//! Sending an import to the daemon (roadmap P2.7): `CHANGE_RULE` only, one
//! rule per notification (opensnitchd reports only the last rule's error of
//! a multi-rule notification), in name order, with at most
//! [`MAX_IN_FLIGHT`] awaiting an answer so the user's own toggles still fit
//! the stream's 64-command queue. Nothing here builds any other action, and
//! `DaemonCommands::send`'s allowlist would refuse one.
//!
//! Just before each send the rule is checked again: by the policy (defence
//! in depth), against the daemon rule the preview compared it with (still
//! the same, or still absent), and the daemon stream must be the one the
//! apply started on. The import stops after [`MAX_UNANSWERED_IN_A_ROW`]
//! rules in a row get no answer.

use super::Replier;
use crate::replier::display_reason;
use snitchwatch_bridge::cache::rules::SharedRulesCache;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands, SendError};
use snitchwatch_bridge::rule_io::{check_rule_for_apply, same_rule, ImportOutcome};
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::{Id, JoinSet};
use tracing::error;

/// Most import commands awaiting the daemon at once.
pub(crate) const MAX_IN_FLIGHT: usize = 8;
/// Unanswered rules in a row after which the import stops.
pub(crate) const MAX_UNANSWERED_IN_A_ROW: u32 = 20;
/// Retries of a command no stream could queue, each after one reply (or
/// `retry_delay` when none is pending).
const SEND_RETRIES: u32 = 3;

const BUSY: &str = "The firewall service was busy, so this rule wasn't sent.";
const NO_DAEMON: &str = "The firewall service isn't connected, so this rule wasn't sent.";
const STREAM_CLOSED: &str =
    "The connection to the firewall service closed, so this rule wasn't sent.";
const RECONNECTED: &str =
    "The firewall service reconnected during the import, so this rule wasn't sent.";
const NOT_ANSWERING: &str = "The firewall service stopped answering, so this rule wasn't sent.";
const ABORTED: &str = "The import stopped because of an internal error, so this rule wasn't sent.";
pub(crate) const CHANGED_SINCE_PREVIEW: &str =
    "This rule changed on the firewall since the preview, so it wasn't sent.";

pub(crate) struct Applier {
    commands: DaemonCommands,
    cache: SharedRulesCache,
    replier: Replier,
    preview_id: String,
    /// HELLO generations: the current one, and the one the apply began on.
    streams: watch::Receiver<u64>,
    started_on: u64,
    reply_timeout: Duration,
    retry_delay: Duration,
}

impl Applier {
    pub(crate) fn new(
        commands: DaemonCommands,
        cache: SharedRulesCache,
        replier: Replier,
        preview_id: String,
        reply_timeout: Duration,
        retry_delay: Duration,
    ) -> Self {
        let streams = commands.stream_ready();
        let started_on = *streams.borrow();
        Self {
            commands,
            cache,
            replier,
            preview_id,
            streams,
            started_on,
            reply_timeout,
            retry_delay,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_reply_timeout(mut self, reply_timeout: Duration) -> Self {
        self.reply_timeout = reply_timeout;
        self
    }

    fn reconnected(&self) -> bool {
        *self.streams.borrow() != self.started_on
    }

    /// Whether the daemon's rule by this name is still what the preview
    /// compared against. Checked under the cache lock, released before the
    /// send (`DaemonCommands` takes its own lock, then the cache's).
    fn unchanged(&self, name: &str, expected: Option<&Rule>) -> bool {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .rules()
            .is_some_and(|rules| same_rule(rules.get(name), expected))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Totals {
    pub(crate) applied: u32,
    pub(crate) rejected: u32,
    pub(crate) not_sent: u32,
    pub(crate) no_answer: u32,
}

/// Send each rule (with the daemon rule its preview compared against) and
/// report its outcome as `RulesImportProgress`, counting into `totals`.
pub(crate) async fn run(
    applier: &Applier,
    mut rules: Vec<(Rule, Option<Rule>)>,
    totals: &mut Totals,
) {
    rules.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    let mut run = Run {
        applier,
        in_flight: JoinSet::new(),
        names: HashMap::new(),
        totals,
        stop: None,
        unanswered: 0,
    };
    for (rule, expected) in rules {
        run.send(rule, expected).await;
    }
    while run.settle_one().await {}
}

type Reply = (String, Result<(), CommandError>);

struct Run<'a> {
    applier: &'a Applier,
    in_flight: JoinSet<Reply>,
    /// Each waiter's rule, should the waiter task itself fail.
    names: HashMap<Id, String>,
    totals: &'a mut Totals,
    /// Set once nothing more may be sent; every later rule gets this reason.
    stop: Option<&'static str>,
    unanswered: u32,
}

impl Run<'_> {
    async fn report(&mut self, name: &str, outcome: ImportOutcome) {
        let count = match &outcome {
            ImportOutcome::Applied => &mut self.totals.applied,
            ImportOutcome::Rejected { .. } | ImportOutcome::Refused { .. } => {
                &mut self.totals.rejected
            }
            ImportOutcome::NoAnswer => &mut self.totals.no_answer,
            ImportOutcome::NotSent { .. } => &mut self.totals.not_sent,
        };
        *count += 1;
        let message = ServerMessage::RulesImportProgress {
            preview_id: self.applier.preview_id.clone(),
            name: name.to_string(),
            outcome,
        };
        self.applier.replier.send(message).await;
    }

    async fn not_sent(&mut self, name: &str, reason: &str) {
        let outcome = ImportOutcome::NotSent {
            reason: reason.to_string(),
        };
        self.report(name, outcome).await;
    }

    /// Wait for one in-flight reply and report it; false when none is left.
    async fn settle_one(&mut self) -> bool {
        let (name, result) = match self.in_flight.join_next_with_id().await {
            None => return false,
            Some(Ok((id, reply))) => {
                self.names.remove(&id);
                reply
            }
            Some(Err(join_error)) => {
                error!(error = %join_error, "an import reply waiter failed");
                let name = self.names.remove(&join_error.id()).unwrap_or_default();
                (name, Err(CommandError::Timeout))
            }
        };
        let outcome = match result {
            Ok(()) => ImportOutcome::Applied,
            Err(CommandError::Rejected(text)) => ImportOutcome::Rejected {
                reason: display_reason(&text),
            },
            Err(CommandError::Timeout) => ImportOutcome::NoAnswer,
            Err(CommandError::StreamClosed) => {
                self.stop.get_or_insert(STREAM_CLOSED);
                ImportOutcome::NoAnswer
            }
        };
        if outcome == ImportOutcome::NoAnswer {
            self.unanswered += 1;
            if self.unanswered >= MAX_UNANSWERED_IN_A_ROW {
                self.stop.get_or_insert(NOT_ANSWERING);
            }
        } else {
            self.unanswered = 0;
        }
        self.report(&name, outcome).await;
        true
    }

    async fn send(&mut self, rule: Rule, expected: Option<Rule>) {
        while self.in_flight.len() >= MAX_IN_FLIGHT {
            self.settle_one().await;
        }
        if self.applier.reconnected() {
            self.stop.get_or_insert(RECONNECTED);
        }
        if let Some(reason) = self.stop {
            return self.not_sent(&rule.name, reason).await;
        }
        if !self.applier.unchanged(&rule.name, expected.as_ref()) {
            return self.not_sent(&rule.name, CHANGED_SINCE_PREVIEW).await;
        }
        let checked = match check_rule_for_apply(&rule) {
            Ok(checked) => checked,
            Err(problems) => {
                let reason = problems
                    .iter()
                    .map(|p| p.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                return self
                    .report(&rule.name, ImportOutcome::Refused { reason })
                    .await;
            }
        };
        let notification = Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![checked],
            ..Default::default()
        };
        self.send_with_retries(rule.name, notification).await;
    }

    async fn send_with_retries(&mut self, name: String, notification: Notification) {
        let mut retries = 0;
        loop {
            match self.applier.commands.send(notification.clone()) {
                Ok(pending) => {
                    let timeout = self.applier.reply_timeout;
                    let waiter_name = name.clone();
                    let handle = self
                        .in_flight
                        .spawn(async move { (waiter_name, pending.wait(timeout).await) });
                    self.names.insert(handle.id(), name);
                    return;
                }
                Err(SendError::NotQueued) if retries < SEND_RETRIES => {
                    retries += 1;
                    if !self.settle_one().await {
                        tokio::time::sleep(self.applier.retry_delay).await;
                    }
                    if let Some(reason) = self.stop {
                        return self.not_sent(&name, reason).await;
                    }
                }
                Err(SendError::NotQueued) => return self.not_sent(&name, BUSY).await,
                Err(
                    refused @ (SendError::InvalidRuleName
                    | SendError::ReservedName
                    | SendError::RefusedOperator),
                ) => {
                    let reason = refused.to_string();
                    return self.report(&name, ImportOutcome::Refused { reason }).await;
                }
                Err(SendError::NoDaemon) => {
                    self.stop = Some(NO_DAEMON);
                    return self.not_sent(&name, NO_DAEMON).await;
                }
                Err(SendError::NotAllowed) => {
                    error!("an import built a command DaemonCommands won't send; stopping");
                    self.stop = Some(ABORTED);
                    return self.not_sent(&name, ABORTED).await;
                }
            }
        }
    }
}
