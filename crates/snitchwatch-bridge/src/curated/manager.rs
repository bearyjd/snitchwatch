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
use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use crate::ws_messages::{ClientMessage, ServerMessage, StorageStatus};

/// How long a curated rule command waits for the daemon's reply.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Why nothing changes when the saved choices can't be read.
pub const UNREADABLE_REASON: &str = "Snitchwatch can't read its saved choices for recommended \
     rules (curated-defaults.json in its state folder), so it adds and removes none: rules \
     already in the firewall were left in place. Repairing the file restores your choices. \
     Without it, Snitchwatch starts with no choices: recommended rules already in the firewall \
     stay as they are until you turn each one on or off.";

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

struct State {
    choices: Choices,
    statuses: BTreeMap<String, EntryStatus>,
    problems: BTreeMap<String, &'static str>,
    /// Commands that failed, by entry id: not sent again until the entry's
    /// choice changes or the daemon reconnects.
    failures: BTreeMap<String, Failure>,
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

/// A failed command for an entry, and when it failed.
#[derive(Debug, Clone, Copy)]
struct Failure {
    /// The daemon stream's generation (`DaemonCommands::stream_ready`).
    generation: u64,
    status: EntryStatus,
    problem: &'static str,
}

/// What a reconcile pass reads; a pass runs only when it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PassKey {
    revision: u64,
    known: bool,
    generation: u64,
    version: u64,
    inert: bool,
    removals: bool,
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
                    failures: BTreeMap::new(),
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
            state.failures.remove(id);
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
            removals: !state.removals.is_empty(),
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
        let daemon = {
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            let left_out: BTreeSet<String> = cache.left_out().keys().cloned().collect();
            cache.rules().cloned().map(|rules| (rules, left_out))
        };
        let Some((rules, left_out)) = daemon else {
            // A removal asked for under the old list doesn't carry over.
            lock(&self.inner.state).removals.clear();
            self.announce_changed();
            return;
        };
        let daemon = DaemonRules {
            rules: &rules,
            left_out: &left_out,
        };
        let generation = self.generation();
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
                let actions = without_failed(&mut state, planned.actions, generation);
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
        for id in removals {
            if self.is_inert() {
                break;
            }
            self.remove(&id, daemon, generation).await;
            self.save_if_changed().await;
            self.announce_changed();
        }
        for action in actions {
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
            CuratedAction::Delete { id, .. } => {
                !state.choices.enabled.contains(id) || !entries().iter().any(|e| &e.id == id)
            }
        }
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
            Err(problem) => {
                let status = match done {
                    Done::Install(_) => EntryStatus::NotInstalled,
                    Done::Delete => EntryStatus::NotRemoved,
                };
                fail(&mut state, &id, status, problem, generation);
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
            Err(problem) => fail(&mut state, id, EntryStatus::NotRemoved, problem, generation),
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
        let Some(job) = self.save_job() else {
            return;
        };
        let inner = self.inner.clone();
        let saved = tokio::task::spawn_blocking(move || save_in_order(&inner, job))
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
            let saved = save_in_order(&self.inner, job);
            self.saved(saved);
        }
    }
}

/// What a confirmed command records.
enum Done {
    Install(Box<snitchwatch_proto::protocol::Rule>),
    Delete,
}

struct SaveJob {
    file: PathBuf,
    choices: Choices,
    version: u64,
}

/// Save `job` unless a newer version is already written; one at a time.
fn save_in_order(inner: &Inner, job: SaveJob) -> std::io::Result<u64> {
    let mut written = inner.saver.lock().unwrap_or_else(|e| e.into_inner());
    if *written >= job.version {
        return Ok(*written);
    }
    store::save(&job.file, &job.choices)?;
    *written = job.version;
    Ok(job.version)
}

/// Take `choices`, if they changed; the worker saves them.
fn keep(state: &mut State, choices: Choices) {
    if state.choices != choices {
        state.choices = choices;
        state.version += 1;
    }
}

/// Record a failed command for `id`.
fn fail(state: &mut State, id: &str, status: EntryStatus, problem: &'static str, generation: u64) {
    state.statuses.insert(id.to_string(), status);
    state.problems.insert(id.to_string(), problem);
    state.failures.insert(
        id.to_string(),
        Failure {
            generation,
            status,
            problem,
        },
    );
}

/// The planned actions, less those that failed for the same entry on the
/// same daemon stream (shown as failed again); an entry with nothing to do
/// forgets its failure.
fn without_failed(
    state: &mut State,
    actions: Vec<CuratedAction>,
    generation: u64,
) -> Vec<CuratedAction> {
    let acted: BTreeSet<String> = actions.iter().map(action_id).collect();
    state
        .failures
        .retain(|id, failure| acted.contains(id) && failure.generation == generation);
    let mut kept = Vec::new();
    for action in actions {
        let id = action_id(&action);
        match state.failures.get(&id).copied() {
            Some(failure) => {
                state.statuses.insert(id.clone(), failure.status);
                state.problems.insert(id, failure.problem);
            }
            None => kept.push(action),
        }
    }
    kept
}

fn action_id(action: &CuratedAction) -> String {
    match action {
        CuratedAction::Install(id) | CuratedAction::Delete { id, .. } => id.clone(),
    }
}

fn summary(entry: &CuratedEntry, state: &State, rules_known: bool) -> CuratedDefaultSummary {
    let status = if rules_known {
        state
            .statuses
            .get(&entry.id)
            .copied()
            .unwrap_or(EntryStatus::Off)
    } else {
        EntryStatus::Waiting
    };
    // An undecided entry already in the firewall reads as on: it is active.
    let on = state.choices.enabled.contains(&entry.id) || status == EntryStatus::InFirewall;
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

#[cfg(test)]
#[path = "manager_loop_tests.rs"]
mod loop_tests;
