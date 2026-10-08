//! The curated defaults worker's state and its bookkeeping: what is
//! remembered between passes, failed commands, saves, and what each entry
//! looks like to a GUI. Split from `manager.rs`, which drives it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;

use super::super::canonical::is_unedited;
use super::super::reconcile::{CuratedAction, DaemonRules, EntryStatus};
use super::super::store::{self, Choices};
use super::super::wire::CuratedDefaultSummary;
use super::super::{entries, CuratedEntry};
use crate::daemon_commands::{CommandError, SendError};
use crate::ws_messages::StorageStatus;

pub(super) struct State {
    pub(super) choices: Choices,
    pub(super) statuses: BTreeMap<String, EntryStatus>,
    pub(super) problems: BTreeMap<String, &'static str>,
    /// Planned commands that failed, by entry id: not sent again until the
    /// entry's choice changes or the daemon reconnects.
    pub(super) failures: BTreeMap<String, Failure>,
    /// Removals the daemon refused, by entry id: shown while the copy is
    /// still edited on the same daemon stream (re-review 2, M2).
    pub(super) removal_failures: BTreeMap<String, Failure>,
    /// Entries the user confirmed removing (edited copies, M2).
    pub(super) removals: BTreeSet<String>,
    /// Bumped by every GUI request taken (a choice, even an unchanged one,
    /// or a removal): each gets a pass (re-review 2, HIGH).
    pub(super) requests: u64,
    pub(super) file: Option<PathBuf>,
    pub(super) storage: StorageStatus,
    /// Why nothing is ever installed or removed here; `None` when it is.
    pub(super) inert: Option<String>,
    /// Bumped on every change of `choices`; `saved` is the last one saved.
    pub(super) version: u64,
    pub(super) saved: u64,
}

impl State {
    pub(super) fn new() -> Self {
        Self {
            choices: Choices::default(),
            statuses: BTreeMap::new(),
            problems: BTreeMap::new(),
            failures: BTreeMap::new(),
            removal_failures: BTreeMap::new(),
            removals: BTreeSet::new(),
            requests: 0,
            file: None,
            storage: StorageStatus {
                persistent: false,
                reason: None,
                unreadable: false,
            },
            inert: None,
            version: 0,
            saved: 0,
        }
    }
}

/// A failed command for an entry, and when it failed.
#[derive(Debug, Clone, Copy)]
pub(super) struct Failure {
    /// The daemon stream's generation (`DaemonCommands::stream_ready`).
    pub(super) generation: u64,
    pub(super) status: EntryStatus,
    pub(super) problem: &'static str,
}

/// Why a command came to nothing, and whether to wait for a new choice or
/// a reconnect before sending it again (`sticky`). A full queue isn't: the
/// gate already keeps it from looping (re-review 2, LOW 3).
#[derive(Debug, Clone, Copy)]
pub(super) struct Problem {
    pub(super) text: &'static str,
    pub(super) sticky: bool,
}

/// What a refusal refused.
#[derive(Clone, Copy)]
pub(super) enum Refusal {
    Add,
    Remove,
}

pub(super) fn send_problem(error: SendError) -> Problem {
    let (text, sticky) = match error {
        SendError::NotQueued => (
            "The firewall service is busy; Snitchwatch tries again when its rule list next \
             changes.",
            false,
        ),
        SendError::NoDaemon => ("The firewall service isn't connected.", true),
        _ => ("Snitchwatch refused to send this rule.", true),
    };
    Problem { text, sticky }
}

pub(super) fn command_problem(error: CommandError, refusal: Refusal) -> Problem {
    let text = match (error, refusal) {
        (CommandError::Rejected(_), Refusal::Add) => "The firewall service refused the rule.",
        (CommandError::Rejected(_), Refusal::Remove) => {
            "The firewall service refused to remove the rule."
        }
        (CommandError::Timeout, _) => "The firewall service didn't answer.",
        (CommandError::StreamClosed, _) => "The firewall service disconnected.",
    };
    Problem { text, sticky: true }
}

/// Take `choices`, if they changed; the worker saves them.
pub(super) fn keep(state: &mut State, choices: Choices) {
    if state.choices != choices {
        state.choices = choices;
        state.version += 1;
    }
}

/// Show a failed command for `id`, and remember it in `failures` unless it
/// may simply be tried again.
pub(super) fn fail(
    state: &mut State,
    removal: bool,
    id: &str,
    status: EntryStatus,
    problem: Problem,
    generation: u64,
) {
    state.statuses.insert(id.to_string(), status);
    state.problems.insert(id.to_string(), problem.text);
    if problem.sticky {
        let failure = Failure {
            generation,
            status,
            problem: problem.text,
        };
        let failures = if removal {
            &mut state.removal_failures
        } else {
            &mut state.failures
        };
        failures.insert(id.to_string(), failure);
    }
}

/// The planned actions, less those that failed for the same entry on the
/// same daemon stream (shown as failed again); an entry with nothing to do
/// forgets its failure.
pub(super) fn without_failed(
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

/// Lay refused removals over the plan's statuses, while each copy is still
/// edited on the same daemon stream.
pub(super) fn show_removal_failures(state: &mut State, daemon: DaemonRules<'_>, generation: u64) {
    state
        .removal_failures
        .retain(|id, failure| failure.generation == generation && still_edited(id, daemon));
    let shown: Vec<(String, Failure)> = state
        .removal_failures
        .iter()
        .map(|(id, failure)| (id.clone(), *failure))
        .collect();
    for (id, failure) in shown {
        state.statuses.insert(id.clone(), failure.status);
        state.problems.insert(id, failure.problem);
    }
}

/// Whether the daemon's copy of `id`'s rule is edited (or too large to
/// read).
pub(super) fn still_edited(id: &str, daemon: DaemonRules<'_>) -> bool {
    let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
        return false;
    };
    let name = entry.rule_name();
    daemon.left_out.contains(&name)
        || daemon
            .rules
            .get(&name)
            .is_some_and(|rule| !is_unedited(Some(entry), None, rule))
}

fn action_id(action: &CuratedAction) -> String {
    match action {
        CuratedAction::Install(id) | CuratedAction::Delete { id, .. } => id.clone(),
    }
}

pub(super) fn summary(
    entry: &CuratedEntry,
    state: &State,
    rules_known: bool,
) -> CuratedDefaultSummary {
    let status = if rules_known {
        state
            .statuses
            .get(&entry.id)
            .copied()
            .unwrap_or(EntryStatus::Off)
    } else {
        EntryStatus::Waiting
    };
    // An undecided entry whose rule is in the firewall and enabled reads as
    // on: it is active. One turned off on the Rules page reads as off.
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

pub(super) struct SaveJob {
    pub(super) file: PathBuf,
    pub(super) choices: Choices,
    pub(super) version: u64,
}

/// Save `job` unless a newer version is already written; one at a time.
/// `written` holds the last version written.
pub(super) fn save_in_order(written: &Mutex<u64>, job: SaveJob) -> std::io::Result<u64> {
    let mut written = written.lock().unwrap_or_else(|e| e.into_inner());
    if *written >= job.version {
        return Ok(*written);
    }
    store::save(&job.file, &job.choices)?;
    *written = job.version;
    Ok(job.version)
}
