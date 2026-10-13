//! The curated defaults worker's state and its bookkeeping: what is
//! remembered between passes, failed commands, saves, and what each entry
//! looks like to a GUI. Split from `manager.rs`, which drives it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::warn;

use super::super::canonical::is_unedited;
use super::super::reconcile::{CuratedAction, DaemonRules, EntryStatus};
use super::super::store::{self, Choices};
use super::super::wire::CuratedDefaultSummary;
use super::super::{entries, CuratedEntry};
use crate::daemon_commands::{CommandError, SendError};
use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use crate::ws_messages::StorageStatus;

pub(super) struct State {
    pub(super) choices: Choices,
    pub(super) broad_revision: u64,
    pub(super) consent_problems: BTreeMap<String, &'static str>,
    pub(super) reenables: BTreeMap<String, u64>,
    pub(super) request_results: Vec<super::intent::PendingResult>,
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
    /// Rule names whose install the daemon refused or didn't answer: stock
    /// `replaceUserRule` takes a rule into memory before `Save` can fail,
    /// so it may apply though the list lacks it (PR #119 review M1), and an
    /// unanswered install may still be taken (#120 item 13). Forgotten once
    /// a delete of the name, or an install, is answered, and at a
    /// reconnect.
    pub(super) maybe_applied: BTreeMap<String, MaybeApplied>,
    /// Entries turned off because the daemon refused their install (#130),
    /// with why: shown until the entry is turned on again.
    pub(super) install_refusals: BTreeMap<String, &'static str>,
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
            broad_revision: 0,
            consent_problems: BTreeMap::new(),
            reenables: BTreeMap::new(),
            request_results: Vec::new(),
            statuses: BTreeMap::new(),
            problems: BTreeMap::new(),
            failures: BTreeMap::new(),
            removal_failures: BTreeMap::new(),
            removals: BTreeSet::new(),
            maybe_applied: BTreeMap::new(),
            install_refusals: BTreeMap::new(),
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

/// A rule that may apply though the list lacks it ([`State::maybe_applied`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaybeApplied {
    /// The daemon stream's generation it was recorded on. A reconnect's
    /// snapshot is the daemon's memory, so it is forgotten then (#120
    /// item 14).
    pub(super) generation: u64,
    /// Whether the rule may have a file, which a refused delete would leave
    /// behind: an unanswered install may have written it, and a refused one
    /// wrote none, so only a file a refused delete left before it counts
    /// (#120 item 15).
    pub(super) file_possible: bool,
}

/// Keep `name`'s [`State::maybe_applied`] record up to date with a
/// command's `outcome`; returns the record an answered command dropped.
/// - A refused or unanswered install records it; `marked` says whether the
///   cache noted a file left behind under the name. A record is never made
///   less cautious on the same stream.
/// - An install or delete answered `OK`, or a refused delete, drops it: the
///   name is out of the daemon's memory or listed. A delete that wasn't
///   answered or sent keeps it.
pub(super) fn track_maybe_applied(
    state: &mut State,
    name: &str,
    install: bool,
    outcome: &Result<(), Problem>,
    marked: bool,
    generation: u64,
) -> Option<MaybeApplied> {
    let (refused, unanswered) = match outcome {
        Ok(()) => (false, false),
        Err(problem) => (problem.daemon_refused, problem.unanswered),
    };
    if install && (refused || unanswered) {
        // Each pass keeps only its own stream's records.
        let earlier = state
            .maybe_applied
            .get(name)
            .is_some_and(|record| record.file_possible);
        let record = MaybeApplied {
            generation,
            file_possible: unanswered || marked || earlier,
        };
        state.maybe_applied.insert(name.to_string(), record);
        None
    } else if outcome.is_ok() || refused {
        state.maybe_applied.remove(name)
    } else {
        None
    }
}

/// Record a delete the daemon answered, `OK` or refused after dropping the
/// rule from memory; returns whether the rule never had a file, so the
/// cache's marker for it is forgotten. `marked`: a file was noted left.
/// - A refusal leaves the file (#119), unless the record `dropped` says the
///   rule never had one (#120 item 15).
/// - `OK` for a `listed` rule removed its file. `OK` for an unlisted name
///   may have touched none: on a daemon that rolls a refused install back
///   (tower #109) the name wasn't in memory, so a file noted before stays
///   (item 19). The bridge can't tell that from stock, where the file went,
///   so it keeps warning (item 10: over-warns only).
/// - The "file may remain" bit is saved with the choices (item 18).
pub(super) fn deleted(
    state: &mut State,
    id: &str,
    refused: bool,
    listed: bool,
    marked: bool,
    dropped: Option<MaybeApplied>,
) -> bool {
    let no_file = dropped.is_some_and(|record| !record.file_possible);
    let file_left = !no_file && (refused || (marked && !listed));
    let removed = state.choices.removed(id);
    let (choices, status) = if file_left {
        warn!(entry = %id, "a recommended rule's file may remain");
        (removed.file_left(id), EntryStatus::OffFileLeft)
    } else if no_file || listed {
        (removed.file_gone(id), EntryStatus::Off)
    } else {
        (removed, EntryStatus::Off)
    };
    keep(state, choices);
    state.failures.remove(id);
    state.statuses.insert(id.to_string(), status);
    no_file
}

/// Names the cache marks as a file left whose record says the rule never
/// had one: a late `ERROR` to a timed-out delete marks it (#120 item 20).
pub(super) fn no_file_marks(state: &State, files_left: &BTreeSet<String>) -> Vec<String> {
    state
        .maybe_applied
        .iter()
        .filter(|(name, record)| !record.file_possible && files_left.contains(*name))
        .map(|(name, _)| name.clone())
        .collect()
}

/// The rule name of the entry `id`.
pub(super) fn rule_name(id: &str) -> String {
    format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{id}")
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
    /// The daemon answered `ERROR`. For a delete that means the rule
    /// already left its memory (`RulesCache::apply_refused`).
    pub(super) daemon_refused: bool,
    /// No answer within the timeout (not a closed stream): the daemon may
    /// still apply the command (#120 item 13).
    pub(super) unanswered: bool,
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
             changes, or when you ask again.",
            false,
        ),
        SendError::NoDaemon => ("The firewall service isn't connected.", true),
        _ => ("Snitchwatch refused to send this rule.", true),
    };
    Problem {
        text,
        sticky,
        daemon_refused: false,
        unanswered: false,
    }
}

pub(super) fn command_problem(error: CommandError, refusal: Refusal) -> Problem {
    let daemon_refused = matches!(error, CommandError::Rejected(_));
    let unanswered = error == CommandError::Timeout;
    let text = match (error, refusal) {
        (CommandError::Rejected(_), Refusal::Add) => "The firewall service refused the rule.",
        (CommandError::Rejected(_), Refusal::Remove) => {
            "The firewall service refused to remove the rule."
        }
        (CommandError::Timeout, _) => "The firewall service didn't answer.",
        (CommandError::StreamClosed, _) => "The firewall service disconnected.",
    };
    Problem {
        text,
        sticky: true,
        daemon_refused,
        unanswered,
    }
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
    let awaiting = status == EntryStatus::AwaitingConsent;
    // Older strict enum decoders know NotInstalled, but not AwaitingConsent.
    let status = if awaiting {
        EntryStatus::NotInstalled
    } else {
        status
    };
    CuratedDefaultSummary {
        id: entry.id.clone(),
        group: entry.group.clone(),
        program: entry.path.clone(),
        allows: entry.allows(),
        why: entry.why.clone(),
        on,
        broad: entry.broad(),
        status,
        problem: if awaiting {
            Some("awaitingConsent: Confirm this broad DNS rule on the Recommended page before installation.".into())
        } else {
            state
                .consent_problems
                .get(&entry.id)
                .or_else(|| state.problems.get(&entry.id))
                .or_else(|| state.install_refusals.get(&entry.id))
                .map(|p| p.to_string())
        },
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
