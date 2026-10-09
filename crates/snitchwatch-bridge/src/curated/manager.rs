//! The curated defaults at run time: the user's choices, reconcile when the
//! rule list or a choice changes, and what every GUI is told (prompt-slot
//! plan Part D, item 13).
//!
//! - Commands go through [`DaemonCommands::send_curated`] and count only on
//!   the daemon's `OK`: an entry reads "Installed" only after it, and its
//!   copy is recorded then.
//! - Reconcile runs one pass at a time on the worker task, on a known rule
//!   list only (never while the rules cache is `Unknown`). The worker wakes
//!   on a change of choice, a committed snapshot and every `SetRules` /
//!   `UpdateRules`, but runs a pass only when something a pass reads
//!   changed ([`PassKey`]): the rule list's revision, its known/withdrawn
//!   state, the daemon stream, the choices, inertness, or a removal asked
//!   for (PR #105 re-review: no hot loop).
//! - A command that fails (refused, not sent, no answer) is not sent again
//!   for that entry until its choice changes or the daemon reconnects with
//!   a new rule list ([`Failure`]). A late `OK` updates the status.
//! - The inbound pump only updates memory: the choices file is written by
//!   the worker, off the async threads, one save at a time, before any
//!   command that depends on it.
//! - **Inert** (code review H1): the per-user bridge, a bridge without
//!   saved settings, choices that can't be read, and choices that can't be
//!   saved all leave the firewall as it is: no command is sent, no choice
//!   is taken, and the GUI is told why.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{broadcast, watch, Notify};
use tracing::{error, info, warn};

use super::canonical::is_unedited;
use super::entries;
use super::reconcile::{inert_statuses, plan, CuratedAction, DaemonRules, EntryStatus};
use super::store;
use crate::cache::rules::SharedRulesCache;
use crate::daemon_commands::{CuratedCommand, DaemonCommands};
use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use crate::ws_messages::{ClientMessage, ServerMessage, StorageStatus};

#[path = "manager_state.rs"]
mod state;
use state::{
    command_problem, fail, keep, save_in_order, send_problem, show_removal_failures, still_edited,
    summary, without_failed, Problem, Refusal, SaveJob, State,
};

/// How long a curated rule command waits for the daemon's reply.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Why nothing changes when the saved choices can't be read.
pub const UNREADABLE_REASON: &str = "Snitchwatch can't read its saved choices for recommended \
     rules (curated-defaults.json in its state folder), so it adds and removes none: rules \
     already in the firewall were left in place. Repair the file, then restart Snitchwatch's \
     background service to restore your choices. Without the file, Snitchwatch starts the next \
     time with no choices: recommended rules already in the firewall stay as they are until you \
     turn each one on or off.";

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
    /// One save at a time; holds the last version written, so an older
    /// snapshot never replaces a newer one (code review LOW-1).
    saver: Mutex<u64>,
    /// Passes run, for the tests of the pass gate.
    #[cfg(test)]
    passes: std::sync::atomic::AtomicU64,
}

/// What a reconcile pass reads; a pass runs only when it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PassKey {
    revision: u64,
    known: bool,
    generation: u64,
    version: u64,
    inert: bool,
    /// GUI requests taken, so each one gets a pass (re-review 2, HIGH).
    requests: u64,
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
                state: Mutex::new(State::new()),
                wake: Notify::new(),
                last: Mutex::new(None),
                saver: Mutex::new(0),
                #[cfg(test)]
                passes: Default::default(),
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

    /// Whether this bridge changes no recommended rules.
    pub fn is_inert(&self) -> bool {
        lock(&self.inner.state).inert.is_some()
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
                state.requests += 1;
                let mut choices = state.choices.clone();
                for entry in entries().iter().filter(|entry| ids.contains(&entry.id)) {
                    choices = if on {
                        choices.enable(&entry.id)
                    } else {
                        choices.disable(&entry.id)
                    };
                    // A new choice: a command that failed may be tried again.
                    state.failures.remove(&entry.id);
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
            state.removal_failures.remove(id);
            state.requests += 1;
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

    fn generation(&self) -> u64 {
        *self.inner.commands.stream_ready().borrow()
    }

    fn pass_key(&self) -> PassKey {
        let (revision, known) = {
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            (cache.revision(), !cache.is_unknown())
        };
        let generation = self.generation();
        let state = lock(&self.inner.state);
        PassKey {
            revision,
            known,
            generation,
            version: state.version,
            inert: state.inert.is_some(),
            requests: state.requests,
        }
    }

    /// Reconcile on a change of choice, a committed snapshot (`synced`,
    /// taken before the gRPC server starts) and any rule-list broadcast,
    /// when a pass's inputs changed.
    pub fn spawn(&self, mut synced: watch::Receiver<u64>) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        // GUIs get the starting state with their snapshot.
        *self.last() = Some(self.message());
        let mut lists = self.inner.broadcast.subscribe();
        let mut ready = self.inner.commands.stream_ready();
        tokio::spawn(async move {
            let mut last_pass: Option<PassKey> = None;
            loop {
                let key = this.pass_key();
                if last_pass.as_ref() != Some(&key) {
                    this.reconcile().await;
                    last_pass = Some(key);
                    // Something changed during the pass: look again.
                    if last_pass.as_ref() != Some(&this.pass_key()) {
                        continue;
                    }
                }
                loop {
                    tokio::select! {
                        () = this.inner.wake.notified() => break,
                        changed = synced.changed() => match changed {
                            Ok(()) => break,
                            Err(_) => return,
                        },
                        changed = ready.changed() => match changed {
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
                // Merge whatever else is queued into this one look.
                loop {
                    match lists.try_recv() {
                        Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                        Err(broadcast::error::TryRecvError::Empty) => break,
                        Err(broadcast::error::TryRecvError::Closed) => return,
                    }
                }
            }
        })
    }

    /// One reconcile pass.
    pub async fn reconcile(&self) {
        #[cfg(test)]
        self.inner
            .passes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.save_if_changed().await;
        // The generation first: a HELLO between the two reads then pairs
        // the new list with the old generation, and the pass stops at the
        // first command (re-review 3, LOW-2).
        let generation = self.generation();
        let daemon = {
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            let left_out: BTreeSet<String> = cache.left_out().keys().cloned().collect();
            let files_left = cache.files_left().clone();
            cache
                .rules()
                .cloned()
                .map(|rules| (rules, left_out, files_left))
        };
        let Some((rules, left_out, files_left)) = daemon else {
            // A removal asked for under the old list doesn't carry over.
            lock(&self.inner.state).removals.clear();
            self.announce_changed();
            return;
        };
        let daemon = DaemonRules {
            rules: &rules,
            left_out: &left_out,
            files_left: &files_left,
        };
        let (actions, removals) = {
            let mut state = lock(&self.inner.state);
            state.problems.clear();
            if state.inert.is_some() {
                state.statuses = inert_statuses(entries(), daemon);
                state.removals.clear();
                state.removal_failures.clear();
                (Vec::new(), BTreeSet::new())
            } else {
                let planned = plan(entries(), daemon, &state.choices);
                state.statuses = planned.statuses;
                keep(&mut state, planned.choices);
                let actions = without_failed(&mut state, planned.actions, generation);
                show_removal_failures(&mut state, daemon, generation);
                (actions, std::mem::take(&mut state.removals))
            }
        };
        self.save_if_changed().await;
        self.announce_changed();
        if actions.is_empty() && removals.is_empty() {
            return;
        }
        // One rule-list broadcast for the pass (if any command changed it).
        let _hold = self.inner.commands.hold_rule_publishes();
        // A reconnect mid-pass ends the pass: its plan is for the old list,
        // and a follow-up pass plans for the new one (re-review 2, M1).
        let mut removals = removals.into_iter();
        while let Some(id) = removals.next() {
            if self.is_inert() {
                break;
            }
            if self.generation() != generation {
                // Asked for, not yet sent: the follow-up pass decides them
                // against the new list.
                let mut state = lock(&self.inner.state);
                state.removals.insert(id);
                state.removals.extend(removals);
                break;
            }
            self.remove(&id, daemon, generation).await;
            self.save_if_changed().await;
            self.announce_changed();
        }
        for action in actions {
            if self.generation() != generation {
                break;
            }
            if !self.still_wanted(&action) {
                continue;
            }
            self.apply(action, generation).await;
            self.save_if_changed().await;
            self.announce_changed();
        }
    }

    /// Whether the user still wants `action` (a choice may change while
    /// earlier commands of the pass wait for the daemon), and, for an
    /// install, whether the daemon still lacks the rule.
    fn still_wanted(&self, action: &CuratedAction) -> bool {
        let state = lock(&self.inner.state);
        if state.inert.is_some() {
            return false;
        }
        match action {
            CuratedAction::Install(id) => {
                let wanted = state.choices.enabled.contains(id)
                    && !state.choices.deleted_by_user.contains(id);
                drop(state);
                wanted && self.still_missing(&format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{id}"))
            }
            CuratedAction::Delete { id, name } => {
                let wanted =
                    !state.choices.enabled.contains(id) || !entries().iter().any(|e| &e.id == id);
                let recorded = state.choices.installed.get(id).cloned();
                drop(state);
                wanted && self.still_unedited(id, name, recorded.as_ref())
            }
        }
    }

    /// The live list's copy under `name` is still unedited: a copy edited
    /// since the pass began is left alone.
    fn still_unedited(
        &self,
        id: &str,
        name: &str,
        recorded: Option<&super::canonical::CanonicalRule>,
    ) -> bool {
        let entry = entries().iter().find(|entry| entry.id == id);
        let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .rules()
            .and_then(|rules| rules.get(name))
            .is_some_and(|rule| is_unedited(entry, recorded, rule))
    }

    /// The live list still lacks `name` (code review LOW-7): a copy that
    /// arrived since the pass began is adopted next pass, not overwritten.
    fn still_missing(&self, name: &str) -> bool {
        let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
        cache.rules().is_some_and(|rules| !rules.contains_key(name))
            && !cache.left_out().contains_key(name)
    }

    async fn apply(&self, action: CuratedAction, generation: u64) {
        let (id, outcome, done) = match action {
            CuratedAction::Install(id) => {
                let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
                    return;
                };
                let outcome = self
                    .send(CuratedCommand::install(entry), Refusal::Add)
                    .await;
                if outcome.is_ok() {
                    info!(entry = %id, "installed a recommended rule");
                }
                (id, outcome, Done::Install(Box::new(entry.rule())))
            }
            CuratedAction::Delete { id, name } => {
                let Some(command) = CuratedCommand::delete(&name) else {
                    return;
                };
                let outcome = self.send(command, Refusal::Remove).await;
                if outcome.is_ok() {
                    info!(entry = %id, "deleted a recommended rule");
                }
                (id, outcome, Done::Delete)
            }
        };
        let mut state = lock(&self.inner.state);
        match outcome {
            Ok(()) => {
                let (choices, status) = match done {
                    Done::Install(rule) => {
                        (state.choices.installed(&id, &rule), EntryStatus::Installed)
                    }
                    Done::Delete => (state.choices.removed(&id), EntryStatus::Off),
                };
                keep(&mut state, choices);
                state.failures.remove(&id);
                state.statuses.insert(id, status);
            }
            // The daemon dropped the rule before failing on its file: it is
            // off, and nothing is sent again (the cache no longer lists it).
            Err(problem) if problem.daemon_refused && matches!(done, Done::Delete) => {
                warn!(entry = %id, "the firewall service couldn't remove a recommended rule's file");
                let choices = state.choices.removed(&id);
                keep(&mut state, choices);
                state.failures.remove(&id);
                state.statuses.insert(id, EntryStatus::OffFileLeft);
            }
            Err(problem) => {
                let status = match done {
                    Done::Install(_) => EntryStatus::NotInstalled,
                    Done::Delete => EntryStatus::NotRemoved,
                };
                fail(&mut state, false, &id, status, problem, generation);
            }
        }
    }

    /// Remove an edited (or unreadable) copy the user confirmed removing,
    /// under exactly that entry's reserved name; an unedited copy is turned
    /// off instead, so nothing is sent for it.
    async fn remove(&self, id: &str, daemon: DaemonRules<'_>, generation: u64) {
        let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
            return;
        };
        let name = entry.rule_name();
        if !still_edited(id, daemon) {
            warn!(entry = %id, "removal asked for a rule that isn't an edited copy; ignored");
            return;
        }
        let Some(command) = CuratedCommand::delete(&name) else {
            return;
        };
        let outcome = self.send(command, Refusal::Remove).await;
        let mut state = lock(&self.inner.state);
        match outcome {
            // A refusal: the daemon dropped the copy before failing on its
            // file, so it is removed as asked, its file left behind.
            Err(problem) if problem.daemon_refused => {
                warn!(entry = %id, "removed an edited recommended rule; its file stays");
                let choices = state.choices.user_removed(id);
                let status = if choices.enabled.contains(id) {
                    EntryStatus::DeletedOutside
                } else {
                    EntryStatus::OffFileLeft
                };
                keep(&mut state, choices);
                state.removal_failures.remove(id);
                state.statuses.insert(id.to_string(), status);
            }
            Ok(()) => {
                info!(entry = %id, "removed an edited recommended rule at the user's request");
                let choices = state.choices.user_removed(id);
                let status = if choices.enabled.contains(id) {
                    EntryStatus::DeletedOutside
                } else {
                    EntryStatus::Off
                };
                keep(&mut state, choices);
                state.removal_failures.remove(id);
                state.statuses.insert(id.to_string(), status);
            }
            Err(problem) => {
                if !problem.sticky {
                    // Busy: asked again at the next change of the pass's
                    // inputs, as its text says (re-review 3).
                    state.removals.insert(id.to_string());
                }
                fail(
                    &mut state,
                    true,
                    id,
                    EntryStatus::NotRemoved,
                    problem,
                    generation,
                );
            }
        }
    }

    async fn send(&self, command: CuratedCommand, refusal: Refusal) -> Result<(), Problem> {
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
        let Some(job) = self.save_job() else {
            return;
        };
        let inner = self.inner.clone();
        let saved = tokio::task::spawn_blocking(move || save_in_order(&inner.saver, job))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
        self.saved(saved);
    }

    fn save_job(&self) -> Option<SaveJob> {
        let state = lock(&self.inner.state);
        match (
            &state.file,
            state.inert.is_none() && state.version != state.saved,
        ) {
            (Some(file), true) => Some(SaveJob {
                file: file.clone(),
                choices: state.choices.clone(),
                version: state.version,
            }),
            _ => None,
        }
    }

    fn saved(&self, saved: std::io::Result<u64>) {
        let mut state = lock(&self.inner.state);
        match saved {
            Ok(version) => state.saved = state.saved.max(version),
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

    /// Save what changed now, blocking (bridge shutdown); never under the
    /// state lock, and never over a newer save.
    pub fn save_now(&self) {
        if let Some(job) = self.save_job() {
            let saved = save_in_order(&self.inner.saver, job);
            self.saved(saved);
        }
    }
}

/// What a confirmed command records.
enum Done {
    Install(Box<snitchwatch_proto::protocol::Rule>),
    Delete,
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "manager_safety_tests.rs"]
mod safety_tests;

#[cfg(test)]
#[path = "manager_loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "manager_gate_tests.rs"]
mod gate_tests;
