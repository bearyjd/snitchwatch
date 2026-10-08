//! Sending an import to the daemon (roadmap P2.7): `CHANGE_RULE` only, one
//! rule per notification (opensnitchd reports only the last rule's error of
//! a multi-rule notification), in name order, with at most
//! [`MAX_IN_FLIGHT`] awaiting an answer so the user's own toggles still fit
//! the stream's 64-command queue. Nothing here builds any other action, and
//! `DaemonCommands::send`'s allowlist would refuse one.

use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands, SendError};
use snitchwatch_bridge::rule_io::{check_rule_for_apply, ImportOutcome};
use snitchwatch_bridge::translator::verdict::sanitize_for_display;
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::{Id, JoinSet};
use tracing::error;

/// Most import commands awaiting the daemon at once.
pub(crate) const MAX_IN_FLIGHT: usize = 8;
/// Retries of a command no stream could queue, each after one reply (or
/// `retry_delay` when none is pending).
const SEND_RETRIES: u32 = 3;
/// Longest daemon error text shown.
const MAX_REASON_CHARS: usize = 200;

const BUSY: &str = "The firewall service was busy, so this rule wasn't sent.";
const NO_DAEMON: &str = "The firewall service isn't connected, so this rule wasn't sent.";
const STREAM_CLOSED: &str =
    "The connection to the firewall service closed, so this rule wasn't sent.";
const ABORTED: &str = "The import stopped because of an internal error, so this rule wasn't sent.";

pub(crate) struct Applier {
    pub(crate) commands: DaemonCommands,
    pub(crate) broadcast: broadcast::Sender<ServerMessage>,
    pub(crate) reply_timeout: Duration,
    pub(crate) retry_delay: Duration,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Totals {
    pub(crate) applied: u32,
    pub(crate) rejected: u32,
    pub(crate) not_sent: u32,
    pub(crate) no_answer: u32,
}

/// Send `rules` and report each one's outcome as `RulesImportProgress`.
pub(crate) async fn run(applier: &Applier, mut rules: Vec<Rule>) -> Totals {
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    let mut run = Run {
        applier,
        in_flight: JoinSet::new(),
        names: HashMap::new(),
        totals: Totals::default(),
        stop: None,
    };
    for rule in rules {
        run.send(rule).await;
    }
    while run.settle_one().await {}
    run.totals
}

type Reply = (String, Result<(), CommandError>);

struct Run<'a> {
    applier: &'a Applier,
    in_flight: JoinSet<Reply>,
    /// Each waiter's rule, should the waiter task itself fail.
    names: HashMap<Id, String>,
    totals: Totals,
    /// Set once nothing more may be sent; every later rule gets this reason.
    stop: Option<&'static str>,
}

impl Run<'_> {
    fn report(&mut self, name: &str, outcome: ImportOutcome) {
        let count = match &outcome {
            ImportOutcome::Applied => &mut self.totals.applied,
            ImportOutcome::Rejected { .. } | ImportOutcome::Refused { .. } => {
                &mut self.totals.rejected
            }
            ImportOutcome::NoAnswer => &mut self.totals.no_answer,
            ImportOutcome::NotSent { .. } => &mut self.totals.not_sent,
        };
        *count += 1;
        let _ = self
            .applier
            .broadcast
            .send(ServerMessage::RulesImportProgress {
                name: name.to_string(),
                outcome,
            });
    }

    fn not_sent(&mut self, name: &str, reason: &str) {
        self.report(
            name,
            ImportOutcome::NotSent {
                reason: reason.to_string(),
            },
        );
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
                reason: sanitize_for_display(&text, MAX_REASON_CHARS),
            },
            Err(CommandError::Timeout) => ImportOutcome::NoAnswer,
            Err(CommandError::StreamClosed) => {
                self.stop.get_or_insert(STREAM_CLOSED);
                ImportOutcome::NoAnswer
            }
        };
        self.report(&name, outcome);
        true
    }

    async fn send(&mut self, rule: Rule) {
        while self.in_flight.len() >= MAX_IN_FLIGHT {
            self.settle_one().await;
        }
        if let Some(reason) = self.stop {
            return self.not_sent(&rule.name, reason);
        }
        let checked = match check_rule_for_apply(&rule) {
            Ok(checked) => checked,
            Err(problems) => {
                let reason = problems
                    .iter()
                    .map(|p| p.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                return self.report(&rule.name, ImportOutcome::Refused { reason });
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
                        return self.not_sent(&name, reason);
                    }
                }
                Err(SendError::NotQueued) => return self.not_sent(&name, BUSY),
                Err(
                    refused @ (SendError::InvalidRuleName
                    | SendError::ReservedName
                    | SendError::RefusedOperator),
                ) => {
                    let reason = refused.to_string();
                    return self.report(&name, ImportOutcome::Refused { reason });
                }
                Err(SendError::NoDaemon) => {
                    self.stop = Some(NO_DAEMON);
                    return self.not_sent(&name, NO_DAEMON);
                }
                Err(SendError::NotAllowed) => {
                    error!("an import built a command DaemonCommands won't send; stopping");
                    self.stop = Some(ABORTED);
                    return self.not_sent(&name, ABORTED);
                }
            }
        }
    }
}
