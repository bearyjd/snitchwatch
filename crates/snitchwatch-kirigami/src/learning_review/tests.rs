//! Tests for the review logic. The helpers build programs the way the
//! recorder keeps them (`Program::normalized`), so keys, displays and the
//! `truncated` mark are the real ones.

use super::*;
use crate::learned::Program;

mod directory;
mod dual_use;
mod flags;
mod present;
mod proposals;
mod rules;
mod suggest;
mod wording;

/// A program at `path` named `name` that made `dests` (destination, count).
pub(super) fn prog(name: &str, path: &str, dests: &[(&str, u64)]) -> Program {
    build(name, Some(path), dests)
}

/// A program the daemon could name but not locate.
pub(super) fn nameless(name: &str, dests: &[(&str, u64)]) -> Program {
    build(name, None, dests)
}

fn build(name: &str, path: Option<&str>, dests: &[(&str, u64)]) -> Program {
    Program {
        raw_name: name.to_string(),
        raw_path: path.map(str::to_string),
        first_seen_ms: 1_000,
        last_seen_ms: 2_000,
        connections: dests.iter().map(|(_, n)| n).sum(),
        destinations: dests.iter().map(|(d, n)| (d.to_string(), *n)).collect(),
        ..Program::default()
    }
    .normalized()
}

/// The proposals for `programs` with the default limits and no rules.
pub(super) fn review(programs: &[Program]) -> Vec<ProgramProposal> {
    build_review(programs, &[], &ReviewConfig::default())
}

/// The one proposal for `program`.
pub(super) fn one(program: Program) -> ProgramProposal {
    let mut all = review(&[program]);
    assert_eq!(all.len(), 1);
    all.remove(0)
}

/// `n` hosts `h0.example.com`.. each seen `count` times.
pub(super) fn hosts(n: usize, count: u64) -> Vec<(String, u64)> {
    (0..n)
        .map(|i| (format!("h{i}.example.com"), count))
        .collect()
}

pub(super) fn prog_with_hosts(path: &str, list: &[(String, u64)]) -> Program {
    let dests: Vec<(&str, u64)> = list.iter().map(|(h, n)| (h.as_str(), *n)).collect();
    prog("app", path, &dests)
}
