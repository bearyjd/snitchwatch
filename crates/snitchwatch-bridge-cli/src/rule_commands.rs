//! Rule commands from GUIs (`AddRule`, `UpdateRule`, `DeleteRule`), with the
//! rule editor's checks and results (roadmap P2.1).
//!
//! Every command is checked before anything is sent ([`gates`]):
//! - an add is a new rule only: its name may not be cached, hidden (left
//!   out of the cache for size) or reserved, and it passes the `Editor`
//!   policy profile (everything the import profile refuses, plus timed
//!   durations within bounds, absolute exact program paths, protocol
//!   tokens);
//! - an update names a cached rule Snitchwatch may change
//!   (`read_only_reason` is `None`: not a blocklist, packaged or curated
//!   rule, not a numeric `user.name` row, not a shape the bridge won't
//!   send back). A pure toggle (only `enabled` differs) is sent as #48 did;
//!   anything else passes the `Editor` profile;
//! - an update to another name is a rename ([`rename`]): `CHANGE_RULE` the
//!   new name, then `DELETE_RULE` the old one;
//! - on the legacy TCP transport only toggles and deletes go through (#35).
//!
//! A command with a valid `request_id` gets one `RuleCommandResult`, sent
//! to the asking connection only. Without one it behaves as #48: no
//! result, and any failure re-sends the rule list to undo a GUI's
//! optimistic change. Only `CHANGE_RULE` and `DELETE_RULE` are ever sent.

use crate::replier::{display_reason, Replier};
use snitchwatch_bridge::cache::rules::{publish_rules, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands, SendError};
use snitchwatch_bridge::rule_policy::RuleProblem;
use snitchwatch_bridge::ws_messages::{
    valid_request_id, ClientMessage, ReplyTo, RuleCommandOutcome, ServerMessage,
};
use snitchwatch_proto::protocol::Notification;
use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

mod gates;
mod rename;

use gates::{Command, Plan};
#[cfg(test)]
pub(crate) use gates::{BUSY, NAME_TAKEN, TCP_REFUSED};

/// How long each daemon answer is awaited (#48's pump uses 5 s).
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The pump's handle for rule commands.
pub(crate) struct RuleCommands {
    commands: DaemonCommands,
    rules: SharedRulesCache,
    broadcast: broadcast::Sender<ServerMessage>,
    reply_timeout: Duration,
    /// Names a rename is working on; no other command may touch them.
    busy: Arc<StdMutex<HashSet<String>>>,
}

/// Where a command's outcome goes.
#[derive(Clone)]
struct Answer {
    request_id: Option<String>,
    replier: Replier,
    rules: SharedRulesCache,
    broadcast: broadcast::Sender<ServerMessage>,
}

impl Answer {
    async fn finish(&self, outcome: RuleCommandOutcome) {
        if outcome != RuleCommandOutcome::Ok {
            // Undo a GUI's optimistic change (#48).
            publish_rules(&self.rules, &self.broadcast);
        }
        if let Some(request_id) = &self.request_id {
            let message = ServerMessage::RuleCommandResult {
                request_id: request_id.clone(),
                outcome,
            };
            self.replier.send(message).await;
        }
    }
}

/// Marks names busy; clears them when dropped.
pub(super) struct BusyNames {
    busy: Arc<StdMutex<HashSet<String>>>,
    names: Vec<String>,
}

impl Drop for BusyNames {
    fn drop(&mut self) {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        for name in &self.names {
            busy.remove(name);
        }
    }
}

impl RuleCommands {
    pub(crate) fn new(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
    ) -> Self {
        Self::with_timeout(commands, rules, broadcast, REPLY_TIMEOUT)
    }

    pub(crate) fn with_timeout(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
        reply_timeout: Duration,
    ) -> Self {
        Self {
            commands,
            rules,
            broadcast,
            reply_timeout,
            busy: Arc::default(),
        }
    }

    /// Handle a rule command (`None`), or hand any other message back.
    /// Never waits: replies are awaited on tasks of their own.
    pub(crate) fn try_route(&self, msg: ClientMessage) -> Option<ClientMessage> {
        let (command, request_id, reply) = match msg {
            ClientMessage::AddRule {
                rule,
                request_id,
                reply,
            } => (Command::Add { rule }, request_id, reply),
            ClientMessage::UpdateRule {
                rule_id,
                rule,
                request_id,
                reply,
            } => (Command::Update { rule_id, rule }, request_id, reply),
            ClientMessage::DeleteRule {
                rule_id,
                request_id,
                reply,
            } => (Command::Delete { rule_id }, request_id, reply),
            other => return Some(other),
        };
        let answer = self.answer(request_id, reply);
        match gates::plan(&command, &self.commands, &self.rules, &self.busy) {
            Err(problems) => {
                warn!(
                    problems = problems.len(),
                    "refused a rule command before sending it"
                );
                tokio::spawn(async move {
                    answer
                        .finish(RuleCommandOutcome::Refused { problems })
                        .await
                });
            }
            Ok(Plan::Send(notification)) => self.send(notification, answer),
            Ok(Plan::Rename { change, old, new }) => {
                let busy = self.mark_busy(vec![old.clone(), new.clone()]);
                let commands = self.commands.clone();
                let timeout = self.reply_timeout;
                tokio::spawn(async move {
                    let outcome = rename::run(&commands, timeout, change, &old, &new).await;
                    drop(busy);
                    answer.finish(outcome).await;
                });
            }
        }
        None
    }

    fn answer(&self, request_id: Option<String>, reply: Option<ReplyTo>) -> Answer {
        Answer {
            request_id: request_id.filter(|id| valid_request_id(id)),
            replier: Replier::new(reply, self.broadcast.clone()),
            rules: self.rules.clone(),
            broadcast: self.broadcast.clone(),
        }
    }

    fn mark_busy(&self, names: Vec<String>) -> BusyNames {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        busy.extend(names.iter().cloned());
        BusyNames {
            busy: self.busy.clone(),
            names,
        }
    }

    fn send(&self, notification: Notification, answer: Answer) {
        let sent = self.commands.send(notification);
        let timeout = self.reply_timeout;
        tokio::spawn(async move {
            let outcome = match sent {
                Ok(pending) => wait_outcome(pending.wait(timeout).await),
                Err(error) => send_error_outcome(error),
            };
            answer.finish(outcome).await;
        });
    }
}

/// A daemon answer as a result.
fn wait_outcome(waited: Result<(), CommandError>) -> RuleCommandOutcome {
    match waited {
        Ok(()) => RuleCommandOutcome::Ok,
        Err(CommandError::Rejected(text)) => RuleCommandOutcome::Rejected {
            reason: display_reason(&text),
        },
        Err(CommandError::Timeout | CommandError::StreamClosed) => RuleCommandOutcome::Timeout,
    }
}

/// Why `DaemonCommands::send` sent nothing, as a result.
fn send_error_outcome(error: SendError) -> RuleCommandOutcome {
    match error {
        SendError::NoDaemon | SendError::NotQueued => RuleCommandOutcome::NoDaemon,
        SendError::NotAllowed
        | SendError::InvalidRuleName
        | SendError::ReservedName
        | SendError::RefusedOperator => RuleCommandOutcome::Refused {
            problems: vec![RuleProblem {
                path: "rule".into(),
                reason: error.to_string(),
            }],
        },
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod rename_tests;
