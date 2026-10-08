//! The curated defaults at run time: the user's choices, reconcile on every
//! committed rules snapshot and on every change of choice, and what every
//! GUI is told (prompt-slot plan Part D, item 13).
//!
//! Commands go through [`DaemonCommands::send_curated`] and count only on
//! the daemon's `OK`: an entry reads "Installed" only after it, and its
//! copy is recorded then. Reconcile runs one at a time, on a known rule
//! list only (never while the rules cache is `Unknown`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{broadcast, watch, Notify};

use super::reconcile::{plan, CuratedAction, EntryStatus};
use super::store::{self, Choices};
use super::wire::CuratedDefaultSummary;
use super::{entries, CuratedEntry};
use crate::cache::rules::SharedRulesCache;
use crate::daemon_commands::{CommandError, CuratedCommand, DaemonCommands, SendError};
use crate::ws_messages::{ClientMessage, ServerMessage, StorageStatus};

/// How long a curated rule command waits for the daemon's reply.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct CuratedDefaults {
    inner: Arc<Inner>,
}

struct Inner {
    commands: DaemonCommands,
    rules: SharedRulesCache,
    broadcast: broadcast::Sender<ServerMessage>,
    state: Mutex<State>,
    wake: Notify,
    /// The last message sent: reconcile tells GUIs only of a change.
    last: Mutex<Option<ServerMessage>>,
}

struct State {
    choices: Choices,
    statuses: BTreeMap<String, EntryStatus>,
    problems: BTreeMap<String, &'static str>,
    file: Option<PathBuf>,
    storage: StorageStatus,
    /// Why nothing is ever installed or removed here.
    unavailable: Option<String>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl CuratedDefaults {
    /// Choices start empty and in memory: nothing is on.
    pub fn new(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                commands,
                rules,
                broadcast,
                state: Mutex::new(State {
                    choices: Choices::default(),
                    statuses: BTreeMap::new(),
                    problems: BTreeMap::new(),
                    file: None,
                    storage: StorageStatus {
                        persistent: false,
                        reason: None,
                        unreadable: false,
                    },
                    unavailable: None,
                }),
                wake: Notify::new(),
                last: Mutex::new(None),
            }),
        }
    }

    /// Keep the choices in `path`, restoring an earlier run's. A file that
    /// can't be read is left alone; the choices stay in memory and the GUI
    /// is told why.
    pub fn attach_file(&self, path: PathBuf) {
        let loaded = store::load(&path);
        let mut state = lock(&self.inner.state);
        match loaded {
            Ok(saved) => {
                state.choices = saved.unwrap_or_default();
                state.file = Some(path);
                state.storage = StorageStatus {
                    persistent: true,
                    reason: None,
                    unreadable: false,
                };
            }
            Err(error) => {
                tracing::error!(%error, "curated defaults' choices are kept in memory only");
                state.storage = StorageStatus {
                    persistent: false,
                    reason: Some(format!("The saved choices couldn't be read: {error}")),
                    unreadable: true,
                };
            }
        }
    }

    /// Keep the choices in memory only, for `status`'s reason.
    pub fn set_storage(&self, status: StorageStatus) {
        lock(&self.inner.state).storage = status;
    }

    /// Never install or remove anything here, and tell GUIs `reason`: the
    /// per-user bridge, or no saved settings (a rule the user deleted
    /// couldn't be remembered). Choices are not taken either.
    pub fn set_unavailable(&self, reason: &str) {
        lock(&self.inner.state).unavailable = Some(reason.to_string());
    }

    /// A GUI's request, if it is one for the curated defaults: applied and
    /// reconciled in the background. Never waits.
    pub fn try_route(&self, msg: ClientMessage) -> Option<ClientMessage> {
        let ClientMessage::SetCuratedDefaults { ids, on } = msg else {
            return Some(msg);
        };
        if lock(&self.inner.state).unavailable.is_some() {
            // Undo the GUI's optimistic switch.
            self.announce();
            return None;
        }
        let known: Vec<&CuratedEntry> = entries()
            .iter()
            .filter(|entry| ids.contains(&entry.id))
            .collect();
        {
            let mut state = lock(&self.inner.state);
            let mut choices = state.choices.clone();
            for entry in known {
                choices = if on {
                    choices.enable(&entry.id)
                } else {
                    choices.disable(&entry.id)
                };
            }
            self.keep(&mut state, choices);
        }
        self.inner.wake.notify_one();
        None
    }

    /// Reconcile again soon, e.g. after a curated rule was turned on or off
    /// in the Rules page (the status shown changes; nothing is sent).
    pub fn refresh(&self) {
        self.inner.wake.notify_one();
    }

    /// The `SetCuratedDefaults` message for the current state.
    pub fn message(&self) -> ServerMessage {
        let state = lock(&self.inner.state);
        let rules_known = !self
            .inner
            .rules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_unknown();
        ServerMessage::SetCuratedDefaults {
            entries: entries()
                .iter()
                .map(|entry| summary(entry, &state, rules_known))
                .collect(),
            storage: state.storage.clone(),
            unavailable: state.unavailable.clone(),
        }
    }

    /// Send the current state to every GUI.
    pub fn announce(&self) {
        let message = self.message();
        *self.last() = Some(message.clone());
        let _ = self.inner.broadcast.send(message);
    }

    /// [`Self::announce`], if the state changed since the last message.
    fn announce_changed(&self) {
        let message = self.message();
        let mut last = self.last();
        if last.as_ref() != Some(&message) {
            *last = Some(message.clone());
            drop(last);
            let _ = self.inner.broadcast.send(message);
        }
    }

    fn last(&self) -> MutexGuard<'_, Option<ServerMessage>> {
        self.inner.last.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reconcile whenever the user changes a choice or a rules snapshot is
    /// committed (`synced`, taken before the gRPC server starts).
    pub fn spawn(&self, mut synced: watch::Receiver<u64>) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        // GUIs get the starting state with their snapshot.
        *self.last() = Some(self.message());
        tokio::spawn(async move {
            loop {
                this.reconcile().await;
                tokio::select! {
                    () = this.inner.wake.notified() => {}
                    changed = synced.changed() => if changed.is_err() { return },
                }
            }
        })
    }

    /// One reconcile pass.
    pub async fn reconcile(&self) {
        let rules = {
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            cache.rules().cloned()
        };
        let unavailable = lock(&self.inner.state).unavailable.is_some();
        let (Some(rules), false) = (rules, unavailable) else {
            self.announce_changed();
            return;
        };
        let actions = {
            let mut state = lock(&self.inner.state);
            let planned = plan(entries(), &rules, &state.choices);
            state.statuses = planned.statuses;
            state.problems.clear();
            self.keep(&mut state, planned.choices);
            planned.actions
        };
        self.announce_changed();
        for action in actions {
            self.apply(action).await;
            self.announce_changed();
        }
    }

    async fn apply(&self, action: CuratedAction) {
        match action {
            CuratedAction::Install(id) => {
                let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
                    return;
                };
                let outcome = self.send(CuratedCommand::install(entry)).await;
                let mut state = lock(&self.inner.state);
                match outcome {
                    Ok(()) => {
                        let choices = state.choices.installed(&id, &entry.rule());
                        self.keep(&mut state, choices);
                        state.statuses.insert(id, EntryStatus::Installed);
                    }
                    Err(problem) => {
                        state.statuses.insert(id.clone(), EntryStatus::NotInstalled);
                        state.problems.insert(id, problem);
                    }
                }
            }
            CuratedAction::Delete { id, name } => {
                let Some(command) = CuratedCommand::delete(&name) else {
                    return;
                };
                let outcome = self.send(command).await;
                let mut state = lock(&self.inner.state);
                match outcome {
                    Ok(()) => {
                        let choices = state.choices.removed(&id);
                        self.keep(&mut state, choices);
                        state.statuses.insert(id, EntryStatus::Off);
                    }
                    Err(problem) => {
                        state.statuses.insert(id.clone(), EntryStatus::NotRemoved);
                        state.problems.insert(id, problem);
                    }
                }
            }
        }
    }

    async fn send(&self, command: CuratedCommand) -> Result<(), &'static str> {
        match self.inner.commands.send_curated(command) {
            Ok(reply) => reply.wait(COMMAND_TIMEOUT).await.map_err(command_problem),
            Err(error) => Err(send_problem(error)),
        }
    }

    /// Take `choices` and save them, if they changed and there is a file.
    fn keep(&self, state: &mut State, choices: Choices) {
        if state.choices == choices {
            return;
        }
        state.choices = choices;
        let Some(file) = state.file.clone() else {
            return;
        };
        if let Err(error) = store::save(&file, &state.choices) {
            tracing::warn!(%error, "curated defaults' choices not saved; kept in memory");
            state.file = None;
            state.storage = StorageStatus {
                persistent: false,
                reason: Some(format!("The choices couldn't be saved: {error}")),
                unreadable: false,
            };
        }
    }
}

fn summary(entry: &CuratedEntry, state: &State, rules_known: bool) -> CuratedDefaultSummary {
    let on = state.choices.enabled.contains(&entry.id);
    let status = if state.unavailable.is_some() {
        EntryStatus::Unavailable
    } else if rules_known {
        state
            .statuses
            .get(&entry.id)
            .copied()
            .unwrap_or(EntryStatus::Off)
    } else {
        EntryStatus::Waiting
    };
    CuratedDefaultSummary {
        id: entry.id.clone(),
        program: entry.path.clone(),
        allows: entry.allows(),
        why: entry.why.clone(),
        on,
        status,
        problem: state.problems.get(&entry.id).map(|p| p.to_string()),
    }
}

fn send_problem(error: SendError) -> &'static str {
    match error {
        SendError::NoDaemon | SendError::NotQueued => "The firewall service isn't connected.",
        _ => "Snitchwatch refused to send this rule.",
    }
}

fn command_problem(error: CommandError) -> &'static str {
    match error {
        CommandError::Rejected(_) => "The firewall service refused the rule.",
        CommandError::Timeout => "The firewall service didn't answer.",
        CommandError::StreamClosed => "The firewall service disconnected.",
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
