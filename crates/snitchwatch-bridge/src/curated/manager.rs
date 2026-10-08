//! The curated defaults at run time: the user's choices, reconcile on every
//! rule-list change and every change of choice, and what every GUI is told
//! (prompt-slot plan Part D, item 13).
//!
//! - Commands go through [`DaemonCommands::send_curated`] and count only on
//!   the daemon's `OK`: an entry reads "Installed" only after it, and its
//!   copy is recorded then.
//! - Reconcile runs one pass at a time on the worker task, on a known rule
//!   list only (never while the rules cache is `Unknown`). It wakes on a
//!   change of choice, a committed snapshot, and every `SetRules` /
//!   `UpdateRules` (a withdrawn list, a late `OK`, a toggle on the Rules
//!   page).
//! - The inbound pump only updates memory: the choices file is written by
//!   the worker, off the async threads, before any command that depends on
//!   it.
//! - **Inert** (code review H1): the per-user bridge, a bridge without
//!   saved settings, choices that can't be read, and choices that can't be
//!   saved all leave the firewall as it is: no command is sent, no choice
//!   is taken, and the GUI is told why.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{broadcast, watch, Notify};
use tracing::{error, info, warn};

use super::canonical::is_unedited;
use super::reconcile::{inert_statuses, plan, CuratedAction, DaemonRules, EntryStatus};
use super::store::{self, Choices};
use super::wire::CuratedDefaultSummary;
use super::{entries, CuratedEntry};
use crate::cache::rules::SharedRulesCache;
use crate::daemon_commands::{CommandError, CuratedCommand, DaemonCommands, SendError};
use crate::ws_messages::{ClientMessage, ServerMessage, StorageStatus};

/// How long a curated rule command waits for the daemon's reply.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Why nothing changes when the saved choices can't be read.
pub const UNREADABLE_REASON: &str = "Snitchwatch can't read its saved choices for recommended \
     rules (curated-defaults.json in its state folder), so it adds and removes none: rules \
     already in the firewall were left in place. Fix or move that file, then restart \
     Snitchwatch's background service.";

/// Why nothing changes once the choices couldn't be saved.
pub const SAVE_FAILED_REASON: &str = "Snitchwatch couldn't save its choices for recommended \
     rules, so it adds and removes none until its background service restarts: rules already \
     in the firewall were left in place.";

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
    /// Entries the user confirmed removing (edited copies, M2).
    removals: BTreeSet<String>,
    file: Option<PathBuf>,
    storage: StorageStatus,
    /// Why nothing is ever installed or removed here; `None` when it is.
    inert: Option<String>,
    /// Bumped on every change of `choices`; `saved` is the last one saved.
    version: u64,
    saved: u64,
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
                    removals: BTreeSet::new(),
                    file: None,
                    storage: StorageStatus {
                        persistent: false,
                        reason: None,
                        unreadable: false,
                    },
                    inert: None,
                    version: 0,
                    saved: 0,
                }),
                wake: Notify::new(),
                last: Mutex::new(None),
            }),
        }
    }

    /// Keep the choices in `path`, restoring an earlier run's. No file is
    /// the first run. A file that can't be read is left alone and the
    /// bridge goes inert, like an unreadable profile store.
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
                error!(%error, "curated defaults: saved choices unreadable; changing nothing");
                state.storage = StorageStatus {
                    persistent: false,
                    reason: Some(error.to_string()),
                    unreadable: true,
                };
                state.inert = Some(UNREADABLE_REASON.to_string());
            }
        }
    }

    /// Where the choices are kept, for a bridge without a file.
    pub fn set_storage(&self, status: StorageStatus) {
        lock(&self.inner.state).storage = status;
    }

    /// Never install or remove anything here, and tell GUIs `reason`.
    pub fn set_unavailable(&self, reason: &str) {
        lock(&self.inner.state).inert = Some(reason.to_string());
    }

    /// A GUI's request, if it is one for the curated defaults: taken in
    /// memory and reconciled on the worker. Never waits, never writes.
    pub fn try_route(&self, msg: ClientMessage) -> Option<ClientMessage> {
        match msg {
            ClientMessage::SetCuratedDefaults { ids, on } => self.choose(&ids, on),
            ClientMessage::RemoveCuratedDefault { id } => self.ask_removal(&id),
            other => return Some(other),
        }
        None
    }

    fn choose(&self, ids: &[String], on: bool) {
        let taken = {
            let mut state = lock(&self.inner.state);
            if state.inert.is_some() {
                false
            } else {
                let mut choices = state.choices.clone();
                for entry in entries().iter().filter(|entry| ids.contains(&entry.id)) {
                    choices = if on {
                        choices.enable(&entry.id)
                    } else {
                        choices.disable(&entry.id)
                    };
                }
                keep(&mut state, choices);
                true
            }
        };
        if taken {
            self.inner.wake.notify_one();
        } else {
            // Undo the GUI's optimistic switch.
            self.announce();
        }
    }

    fn ask_removal(&self, id: &str) {
        let mut state = lock(&self.inner.state);
        if state.inert.is_none() && entries().iter().any(|entry| entry.id == id) {
            state.removals.insert(id.to_string());
            drop(state);
            self.inner.wake.notify_one();
        }
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
            unavailable: state.inert.clone(),
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

    /// Reconcile on a change of choice, a committed snapshot (`synced`,
    /// taken before the gRPC server starts) and any rule-list broadcast.
    pub fn spawn(&self, mut synced: watch::Receiver<u64>) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        // GUIs get the starting state with their snapshot.
        *self.last() = Some(self.message());
        let mut lists = self.inner.broadcast.subscribe();
        tokio::spawn(async move {
            loop {
                this.reconcile().await;
                loop {
                    tokio::select! {
                        () = this.inner.wake.notified() => break,
                        changed = synced.changed() => match changed {
                            Ok(()) => break,
                            Err(_) => return,
                        },
                        message = lists.recv() => match message {
                            Ok(ServerMessage::SetRules { .. } | ServerMessage::UpdateRules { .. })
                            | Err(broadcast::error::RecvError::Lagged(_)) => break,
                            Ok(_) => {}
                            Err(broadcast::error::RecvError::Closed) => return,
                        },
                    }
                }
            }
        })
    }

    /// One reconcile pass.
    pub async fn reconcile(&self) {
        self.save_if_changed().await;
        let daemon = {
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            let left_out: BTreeSet<String> = cache.left_out().keys().cloned().collect();
            cache.rules().cloned().map(|rules| (rules, left_out))
        };
        let Some((rules, left_out)) = daemon else {
            self.announce_changed();
            return;
        };
        let daemon = DaemonRules {
            rules: &rules,
            left_out: &left_out,
        };
        let (actions, removals) = {
            let mut state = lock(&self.inner.state);
            state.problems.clear();
            if state.inert.is_some() {
                state.statuses = inert_statuses(entries(), daemon);
                state.removals.clear();
                (Vec::new(), BTreeSet::new())
            } else {
                let planned = plan(entries(), daemon, &state.choices);
                state.statuses = planned.statuses;
                keep(&mut state, planned.choices);
                (planned.actions, std::mem::take(&mut state.removals))
            }
        };
        self.save_if_changed().await;
        self.announce_changed();
        if actions.is_empty() && removals.is_empty() {
            return;
        }
        // One rule-list broadcast for the pass, like a rule import.
        let _hold = self.inner.commands.hold_rule_publishes();
        for id in removals {
            if self.is_inert() {
                break;
            }
            self.remove(&id, daemon).await;
            self.save_if_changed().await;
            self.announce_changed();
        }
        for action in actions {
            if !self.still_wanted(&action) {
                continue;
            }
            self.apply(action).await;
            self.save_if_changed().await;
            self.announce_changed();
        }
    }

    /// Whether this bridge changes no recommended rules (see [`Self::set_unavailable`]).
    pub fn is_inert(&self) -> bool {
        lock(&self.inner.state).inert.is_some()
    }

    /// Whether the user still wants `action` (a choice may change while
    /// earlier commands of the pass wait for the daemon).
    fn still_wanted(&self, action: &CuratedAction) -> bool {
        let state = lock(&self.inner.state);
        if state.inert.is_some() {
            return false;
        }
        match action {
            CuratedAction::Install(id) => {
                state.choices.enabled.contains(id) && !state.choices.deleted_by_user.contains(id)
            }
            CuratedAction::Delete { id, .. } => {
                !state.choices.enabled.contains(id) || !entries().iter().any(|e| &e.id == id)
            }
        }
    }

    async fn apply(&self, action: CuratedAction) {
        match action {
            CuratedAction::Install(id) => {
                let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
                    return;
                };
                let outcome = self
                    .send(CuratedCommand::install(entry), Refusal::Add)
                    .await;
                let mut state = lock(&self.inner.state);
                match outcome {
                    Ok(()) => {
                        info!(entry = %id, "installed a recommended rule");
                        let choices = state.choices.installed(&id, &entry.rule());
                        keep(&mut state, choices);
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
                let outcome = self.send(command, Refusal::Remove).await;
                let mut state = lock(&self.inner.state);
                match outcome {
                    Ok(()) => {
                        info!(entry = %id, "deleted a recommended rule");
                        let choices = state.choices.removed(&id);
                        keep(&mut state, choices);
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

    /// Remove an edited (or unreadable) copy the user confirmed removing,
    /// under exactly that entry's reserved name; an unedited copy is turned
    /// off instead, so nothing is sent for it.
    async fn remove(&self, id: &str, daemon: DaemonRules<'_>) {
        let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
            return;
        };
        let name = entry.rule_name();
        let edited = daemon.left_out.contains(&name)
            || daemon
                .rules
                .get(&name)
                .is_some_and(|rule| !is_unedited(Some(entry), None, rule));
        if !edited {
            warn!(entry = %id, "removal asked for a rule that isn't an edited copy; ignored");
            return;
        }
        let Some(command) = CuratedCommand::delete(&name) else {
            return;
        };
        let outcome = self.send(command, Refusal::Remove).await;
        let mut state = lock(&self.inner.state);
        match outcome {
            Ok(()) => {
                info!(entry = %id, "removed an edited recommended rule at the user's request");
                let choices = state.choices.user_removed(id);
                let status = if choices.enabled.contains(id) {
                    EntryStatus::DeletedOutside
                } else {
                    EntryStatus::Off
                };
                keep(&mut state, choices);
                state.statuses.insert(id.to_string(), status);
            }
            Err(problem) => {
                state
                    .statuses
                    .insert(id.to_string(), EntryStatus::NotRemoved);
                state.problems.insert(id.to_string(), problem);
            }
        }
    }

    async fn send(&self, command: CuratedCommand, refusal: Refusal) -> Result<(), &'static str> {
        match self.inner.commands.send_curated(command) {
            Ok(reply) => reply
                .wait(COMMAND_TIMEOUT)
                .await
                .map_err(|error| command_problem(error, refusal)),
            Err(error) => Err(send_problem(error)),
        }
    }

    /// Write the choices if they changed, on a blocking thread. A failure
    /// makes the bridge inert: it must not act on choices it can't keep.
    async fn save_if_changed(&self) {
        let job = {
            let state = lock(&self.inner.state);
            match (
                &state.file,
                state.inert.is_none() && state.version != state.saved,
            ) {
                (Some(file), true) => Some((file.clone(), state.choices.clone(), state.version)),
                _ => None,
            }
        };
        let Some((file, choices, version)) = job else {
            return;
        };
        let saved = tokio::task::spawn_blocking(move || store::save(&file, &choices))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
        let mut state = lock(&self.inner.state);
        match saved {
            Ok(()) => state.saved = state.saved.max(version),
            Err(error) => {
                error!(%error, "curated defaults: choices not saved; changing nothing");
                state.file = None;
                state.storage = StorageStatus {
                    persistent: false,
                    reason: Some(error.to_string()),
                    unreadable: false,
                };
                state.inert = Some(SAVE_FAILED_REASON.to_string());
            }
        }
    }

    /// Save what changed now, blocking (bridge shutdown).
    pub fn save_now(&self) {
        let state = lock(&self.inner.state);
        if let (Some(file), None, true) = (&state.file, &state.inert, state.version != state.saved)
        {
            if let Err(error) = store::save(file, &state.choices) {
                error!(%error, "curated defaults: choices not saved at shutdown");
            }
        }
    }
}

/// Take `choices`, if they changed; the worker saves them.
fn keep(state: &mut State, choices: Choices) {
    if state.choices != choices {
        state.choices = choices;
        state.version += 1;
    }
}

fn summary(entry: &CuratedEntry, state: &State, rules_known: bool) -> CuratedDefaultSummary {
    let on = state.choices.enabled.contains(&entry.id);
    let status = if rules_known {
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

/// What a refusal refused.
#[derive(Clone, Copy)]
enum Refusal {
    Add,
    Remove,
}

fn send_problem(error: SendError) -> &'static str {
    match error {
        SendError::NoDaemon | SendError::NotQueued => "The firewall service isn't connected.",
        _ => "Snitchwatch refused to send this rule.",
    }
}

fn command_problem(error: CommandError, refusal: Refusal) -> &'static str {
    match (error, refusal) {
        (CommandError::Rejected(_), Refusal::Add) => "The firewall service refused the rule.",
        (CommandError::Rejected(_), Refusal::Remove) => {
            "The firewall service refused to remove the rule."
        }
        (CommandError::Timeout, _) => "The firewall service didn't answer.",
        (CommandError::StreamClosed, _) => "The firewall service disconnected.",
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "manager_safety_tests.rs"]
mod safety_tests;
