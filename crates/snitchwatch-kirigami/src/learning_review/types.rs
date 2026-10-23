//! The review's value types.

use crate::app_names::ProgramName;
use crate::learned::PathFlag;

/// How many destinations a row lists by default (a high-fan-out program
/// lists none: see [`Destinations::hidden`]).
pub const SUMMARY_TOP: usize = 5;

/// Fixed sentence for [`ProgramProposal::block_warning`].
pub const BLOCK_WARNING: &str = "Blocking this may stop networking or updates.";

/// Fixed sentence for [`ProgramProposal::allow_any_warning`].
pub const DUAL_USE_WARNING: &str = "This program can run other programs or scripts. Allowing any destination lets all of them connect anywhere.";

/// The tunable limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewConfig {
    /// From this many distinct destinations a program is high fan-out.
    pub fan_out_threshold: usize,
    /// The most hosts "only these destinations" lists; more offers none.
    pub max_hosts: usize,
    /// A host must have been seen this many times to be listed.
    pub min_host_sightings: u64,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            fan_out_threshold: 25,
            max_hosts: 10,
            min_host_sightings: 2,
        }
    }
}

/// The objective facts about a program. `(n, of)` pairs are "n of `of`
/// connections".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    /// Runs from a user-writable or temporary directory, or its file was
    /// deleted (`path_flag` says which).
    pub user_writable_path: bool,
    pub path_flag: Option<PathFlag>,
    pub bare_ip: (u64, u64),
    pub uncommon_port: (u64, u64),
    /// Its name equals a well-known system program but it does not run from
    /// the system's own directories.
    pub name_mimics_system: bool,
}

/// Why no rule can be made for a program. Fixed sentences ([`Self::text`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonSelectable {
    NoPath,
    NotBindable,
    Deleted,
    Truncated,
}

/// The directory rule for a versioned binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRule {
    /// `<dir>/<app>/versions/`, as the owner sees it.
    pub folder: String,
    /// The `process.path` regular expression.
    pub pattern: String,
    /// Always true: the match is case-sensitive.
    pub sensitive: bool,
    /// The folder is one the user (or a program running as the user) can
    /// write to.
    pub user_writable: bool,
}

/// What the owner can choose for a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Any destination: one rule on the exact `process.path`.
    AllowAny,
    /// Only these destinations: one rule per entry.
    AllowHosts(Vec<String>),
    /// Deny the exact `process.path`, any destination.
    Block,
    /// Any file in a versions folder.
    DirectoryRule(DirectoryRule),
    /// No rule can be made.
    NonSelectable(NonSelectable),
}

/// A kind of [`Choice`], for matching without the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    AllowAny,
    AllowHosts,
    Block,
    DirectoryRule,
    NonSelectable,
}

/// The destinations a row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destinations {
    /// Connections recorded.
    pub connections: u64,
    /// Distinct destinations, counting the ones past the recorder's limit
    /// (an upper bound when `!exact`).
    pub distinct: usize,
    pub exact: bool,
    /// The busiest [`SUMMARY_TOP`] with their counts; empty when `hidden`.
    pub top: Vec<(String, u64)>,
    /// A high-fan-out program's list is left out until asked for
    /// ([`ProgramProposal::top_destinations`]).
    pub hidden: bool,
}

/// Everything the review shows and offers for one program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramProposal {
    /// The recorder's key (`path:...` or `name:...`).
    pub key: String,
    /// The 0a label and whether it needs its "unverified" marker.
    pub name: ProgramName,
    pub marker: bool,
    /// The sanitised path (else name), and its last components.
    pub display: String,
    pub path_tail: String,
    pub flags: Flags,
    pub connections: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub destinations: Destinations,
    /// Distinct destinations including the ones past the recorder's limit.
    pub fan_out: usize,
    pub high_fan_out: bool,
    pub dual_use: bool,
    /// What can be chosen. Never ticked by default.
    pub choices: Vec<Choice>,
    /// Choices an enabled rule already makes (best effort); not offered.
    pub already_present: Vec<Choice>,
    /// "Select suggested" ticks this program's [`Choice::AllowHosts`].
    pub selected_by_suggest: bool,
    pub block_warning: Option<String>,
    pub allow_any_warning: Option<String>,
    pub(super) all_destinations: Vec<(String, u64)>,
}

impl ProgramProposal {
    /// The flags as plain facts, one sentence each.
    pub fn describe_flags(&self) -> Vec<String> {
        self.flags.describe()
    }

    /// The `n` busiest destinations with their counts, on demand (also for a
    /// high-fan-out program, whose list is not in [`Destinations::top`]).
    pub fn top_destinations(&self, n: usize) -> Vec<(String, u64)> {
        self.all_destinations.iter().take(n).cloned().collect()
    }

    /// What "Select suggested" ticks for this program: its
    /// [`Choice::AllowHosts`] when [`Self::selected_by_suggest`], else nothing.
    /// Never "any destination", Block or a directory rule.
    pub fn suggested_choice(&self) -> Option<&Choice> {
        self.selected_by_suggest
            .then(|| self.choice(ChoiceKind::AllowHosts))
            .flatten()
    }

    /// The offered choice of `kind`.
    pub fn choice(&self, kind: ChoiceKind) -> Option<&Choice> {
        self.choices.iter().find(|c| c.kind() == kind)
    }
}

impl Choice {
    pub fn kind(&self) -> ChoiceKind {
        match self {
            Self::AllowAny => ChoiceKind::AllowAny,
            Self::AllowHosts(_) => ChoiceKind::AllowHosts,
            Self::Block => ChoiceKind::Block,
            Self::DirectoryRule(_) => ChoiceKind::DirectoryRule,
            Self::NonSelectable(_) => ChoiceKind::NonSelectable,
        }
    }
}

impl NonSelectable {
    /// Why no rule can be made, in plain words.
    pub fn text(self) -> &'static str {
        match self {
            Self::NoPath => "cannot make a rule: the program path is unknown",
            Self::NotBindable => "cannot make a rule: this is not the path of a program file",
            Self::Deleted => "cannot make a rule: the program file has been deleted",
            Self::Truncated => {
                "cannot make a rule: the program path was too long or odd to keep exactly"
            }
        }
    }
}

impl DirectoryRule {
    /// The rule in plain words, for the owner.
    pub fn text(&self) -> String {
        if self.user_writable {
            format!(
                "ANY file in this folder: {} (a folder your user can write to)",
                self.folder
            )
        } else {
            format!("ANY file in this folder: {}", self.folder)
        }
    }
}
