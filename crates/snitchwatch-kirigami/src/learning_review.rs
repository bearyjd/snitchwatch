//! Learning mode's review logic (phase L3): what the owner is offered for
//! each program the recorder saw. PURE: no UI, no I/O, no Qt, and nothing
//! here creates or sends a rule ([`rules_for`] only builds the payload a
//! later phase would send, after the owner's explicit tick).
//!
//! * `build` turns the recorded programs into [`ProgramProposal`]s, busiest
//!   first. NOTHING is ticked by default. [`ProgramProposal::selected_by_suggest`]
//!   is the one thing "Select suggested" may tick, and it is true ONLY for the
//!   narrow "only these destinations" choice of an unflagged, not dual-use,
//!   not high-fan-out program that runs from a file directly under a system
//!   directory. It is never true for "any destination", Block or a directory
//!   rule, and a desktop-entry match is never an input.
//! * `flags` are objective facts (a path, a count), never verdicts: a guard
//!   test scans every string literal of this module for verdict words.
//! * `dual_use` is the closed, tested list of programs that run other
//!   programs or reach anywhere (interpreters, shells, network tools).
//! * `directory` is the versioned-binary directory rule, `rule` builds the
//!   payloads (through the bridge's own builder, so they have the shape
//!   `make_rule.rs` sends) and `present` finds a proposal an existing rule
//!   already makes.

mod build;
mod directory;
mod dual_use;
mod flags;
mod paths;
mod present;
mod rule;
mod types;

pub use build::{build_review, programs_of};
pub use directory::directory_rule;
pub use dual_use::is_dual_use;
pub use paths::{is_direct_system_path, is_system_path, selectability};
pub use rule::{rules_for, BuildRefusal, RulePayload, ALLOW_NAME_PREFIX, BLOCK_NAME_PREFIX};
pub use types::*;

#[cfg(test)]
mod tests;
