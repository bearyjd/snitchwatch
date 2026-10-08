//! Rule commands from GUIs (`AddRule`, `UpdateRule`, `DeleteRule`), with the
//! rule editor's checks and results (roadmap P2.1).
//!
//! Every command is checked before anything is sent ([`gates`]):
//! - an add is a new rule only: its name may not be cached, hidden (left
//!   out of the cache for size), reserved or busy, and it passes the
//!   `Editor` policy profile (everything the import profile refuses, plus
//!   timed durations within bounds, absolute exact program paths, protocol
//!   tokens). Its name stays busy until the daemon answers;
//! - an update names a cached rule Snitchwatch may change
//!   (`read_only_reason` is `None`: not a blocklist, profile, packaged or
//!   curated rule, not a numeric `user.name` row, not a shape the bridge
//!   won't send back). A pure toggle (only `enabled` differs) is sent as
//!   #48 did, except that turning a rule on runs the checks that keep a
//!   rule from matching everything ([`gates`]). A recommended background-
//!   service rule (prompt-slot D) may only be toggled, and the bridge sends
//!   the data file's rule, not the GUI's; anything else passes the
//!   `Editor` profile ([`edit`] restores a rule the daemon dropped the file
//!   of);
//! - an update to another name is a rename ([`rename`]): `CHANGE_RULE` the
//!   new name, then `DELETE_RULE` the old one;
//! - on the legacy TCP transport only toggles and deletes go through (#35).
//!
//! A command with a valid `request_id` gets one `RuleCommandResult`, sent
//! to the asking connection only. Without one it behaves as #48: no
//! result, and any failure re-sends the rule list to undo a GUI's
//! optimistic change. Only `CHANGE_RULE` and `DELETE_RULE` are ever sent.

use crate::busy::{BusyGuard, BusyNames};
use crate::replier::{display_reason, Replier};
use snitchwatch_bridge::cache::rules::{publish_rules, SharedRulesCache};
use snitchwatch_bridge::curated::manager::CuratedDefaults;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands, SendError};
use snitchwatch_bridge::rule_policy::RuleProblem;
use snitchwatch_bridge::ws_messages::{
    valid_request_id, ClientMessage, ReplyTo, RuleCommandOutcome, ServerMessage,
};
use snitchwatch_proto::protocol::Notification;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

mod edit;
mod gates;
mod rename;
mod steps;

use gates::{Command, Plan};
#[cfg(test)]
pub(crate) use gates::{BUSY, CURATED_INERT_REFUSED, NAME_TAKEN, TCP_REFUSED};

/// How long each daemon answer is awaited (#48's pump uses 5 s).
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The pump's handle for rule commands.
pub(crate) struct RuleCommands {
    commands: DaemonCommands,
    rules: SharedRulesCache,
    broadcast: broadcast::Sender<ServerMessage>,
    reply_timeout: Duration,
    /// Names a command or an import is working on; no other may touch them.
    busy: BusyNames,
    /// The recommended rules: their toggles are refused while it is inert.
    curated: Option<CuratedDefaults>,
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
    /// Undo a GUI's optimistic change (#48) after anything but a clean OK;
    /// the result goes only to a request that asked for one.
    fn republish(&self, outcome: &RuleCommandOutcome) {
        if *outcome != RuleCommandOutcome::Ok {
            publish_rules(&self.rules, &self.broadcast);
        }
    }

    fn result(&self, outcome: RuleCommandOutcome) -> Option<ServerMessage> {
        self.request_id
            .clone()
            .map(|request_id| ServerMessage::RuleCommandResult {
                request_id,
                outcome,
            })
    }

    /// After the daemon answered: waits briefly for room, even on a stalled
    /// connection.
    async fn finish(&self, outcome: RuleCommandOutcome) {
        self.republish(&outcome);
        if let Some(message) = self.result(outcome) {
            self.replier.send_final(message).await;
        }
    }

    /// Refused before sending: answered at once, never by a task of its
    /// own. Only a request without an id (#48's optimistic GUIs) needs the
    /// rule list again; a refused result is dropped if there is no room.
    fn refuse(&self, problems: Vec<RuleProblem>) {
        let outcome = RuleCommandOutcome::Refused { problems };
        match self.result(outcome.clone()) {
            Some(message) => self.replier.send_now(message),
            None => self.republish(&outcome),
        }
    }
}

impl RuleCommands {
    pub(crate) fn new(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
        busy: BusyNames,
    ) -> Self {
        Self::with_timeout(commands, rules, broadcast, busy, REPLY_TIMEOUT)
    }

    pub(crate) fn with_timeout(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
        busy: BusyNames,
        reply_timeout: Duration,
    ) -> Self {
        Self {
            commands,
            rules,
            broadcast,
            reply_timeout,
            busy,
            curated: None,
        }
    }

    /// Refuse toggles of recommended rules while `curated` changes none.
    pub(crate) fn with_curated(self, curated: CuratedDefaults) -> Self {
        Self {
            curated: Some(curated),
            ..self
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
        let curated_inert = self.curated.as_ref().is_some_and(CuratedDefaults::is_inert);
        match gates::plan(
            &command,
            &self.commands,
            &self.rules,
            &self.busy,
            curated_inert,
        ) {
            Err(problems) => {
                warn!(problems = problems.len(), "refused a rule command");
                answer.refuse(problems);
            }
            Ok(plan) => self.run(plan, answer),
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

    /// Claim `names` for the command's duration, or refuse it as busy.
    fn claim(&self, names: &[&str], answer: &Answer) -> Option<BusyGuard> {
        let guard = self.busy.claim(names);
        if guard.is_none() {
            answer.refuse(vec![RuleProblem {
                path: "name".into(),
                reason: gates::BUSY.into(),
            }]);
        }
        guard
    }

    fn run(&self, plan: Plan, answer: Answer) {
        let (task, guard) = match plan {
            Plan::Send(notification) => return self.send(notification, answer),
            Plan::ToggleCurated { name, enabled } => {
                return self.toggle_curated(&name, enabled, answer)
            }
            Plan::Add { change, name } => match self.claim(&[&name], &answer) {
                Some(guard) => (Task::Send(change), Some(guard)),
                None => return,
            },
            Plan::Edit { change, old } => match self.claim(&[&old.name], &answer) {
                Some(guard) => (Task::Edit(change, old), Some(guard)),
                None => return,
            },
            Plan::Rename { change, old } => {
                let new = change.rules[0].name.clone();
                match self.claim(&[&old.name, &new], &answer) {
                    Some(guard) => (Task::Rename(change, old), Some(guard)),
                    None => return,
                }
            }
        };
        let (commands, rules) = (self.commands.clone(), self.rules.clone());
        let timeout = self.reply_timeout;
        tokio::spawn(async move {
            let outcome = match task {
                Task::Send(change) => sent_outcome(commands.send(change), timeout).await,
                Task::Edit(change, old) => {
                    edit::run(&commands, &rules, timeout, change, &old).await
                }
                Task::Rename(change, old) => {
                    rename::run(&commands, &rules, timeout, change, &old).await
                }
            };
            drop(guard);
            answer.finish(outcome).await;
        });
    }

    fn send(&self, notification: Notification, answer: Answer) {
        let sent = self.commands.send(notification);
        let timeout = self.reply_timeout;
        tokio::spawn(async move {
            let outcome = sent_outcome(sent, timeout).await;
            answer.finish(outcome).await;
        });
    }

    /// A recommended rule turned on or off. The daemon's `OK` updates the
    /// rule list, whose broadcast also refreshes the Recommended page.
    fn toggle_curated(&self, name: &str, enabled: bool, answer: Answer) {
        let sent = self.commands.send_curated_toggle(name, enabled);
        let timeout = self.reply_timeout;
        tokio::spawn(async move {
            let outcome = sent_outcome(sent, timeout).await;
            answer.finish(outcome).await;
        });
    }
}

/// The multi-step work of a planned command.
enum Task {
    Send(Notification),
    Edit(Notification, snitchwatch_proto::protocol::Rule),
    Rename(Notification, snitchwatch_proto::protocol::Rule),
}

async fn sent_outcome(
    sent: Result<snitchwatch_bridge::daemon_commands::PendingReply, SendError>,
    timeout: Duration,
) -> RuleCommandOutcome {
    match sent {
        Ok(pending) => wait_outcome(pending.wait(timeout).await),
        Err(error) => send_error_outcome(error),
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

#[cfg(test)]
mod edit_tests;

#[cfg(test)]
mod toggle_tests;

#[cfg(test)]
mod curated_toggle_tests;
