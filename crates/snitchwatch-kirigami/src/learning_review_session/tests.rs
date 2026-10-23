//! Tests for the review session. Programs are built the way the recorder
//! keeps them (`Program::normalized`) and put through a real `Aggregate`.

use super::*;
use crate::learned::Program;
use crate::learning_review::ChoiceKind as K;

mod batch;
mod json;
mod summary;
mod ticks;
mod wording;

pub(super) fn program(name: &str, path: Option<&str>, dests: &[(&str, u64)]) -> Program {
    Program {
        raw_name: name.to_string(),
        raw_path: path.map(str::to_string),
        first_seen_ms: 1_000,
        last_seen_ms: 2_000,
        connections: dests.iter().map(|(_, n)| n).sum(),
        destinations: dests.iter().map(|(d, n)| (d.to_string(), *n)).collect(),
        ..Program::default()
    }
}

/// A session over `programs`, with no rules list.
pub(super) fn session(programs: Vec<Program>) -> ReviewSession {
    let mut aggregate = Aggregate::default();
    for program in programs {
        aggregate.absorb(program);
    }
    ReviewSession::new(&aggregate, &[], true)
}

/// Unflagged, direct system path, two hosts seen twice: "Select suggested"
/// takes it.
pub(super) fn narrow(name: &str) -> Program {
    program(
        name,
        Some(&format!("/usr/bin/{name}")),
        &[("a.example", 2), ("b.example", 3)],
    )
}

/// A user-writable path: never suggested.
pub(super) fn tmp(name: &str) -> Program {
    program(name, Some(&format!("/tmp/{name}")), &[("a.example", 2)])
}

/// A dual-use program (an interpreter).
pub(super) fn dual(name: &str) -> Program {
    program(name, Some(&format!("/usr/bin/{name}")), &[("a.example", 2)])
}

pub(super) fn key_of(session: &ReviewSession, path: &str) -> String {
    session
        .proposals()
        .iter()
        .find(|p| p.key == format!("path:{path}"))
        .unwrap_or_else(|| panic!("no row for {path}"))
        .key
        .clone()
}

#[test]
fn the_empty_session_has_no_rows_and_no_ticks() {
    let s = ReviewSession::empty();
    assert!(s.proposals().is_empty());
    assert_eq!(s.tick_count(), 0);
    assert_eq!(s.rows_json(), "[]");
    assert_eq!(s.summary_json(), "null");
    assert_eq!(s.freeze().unwrap_err(), Refusal::NothingTicked);
}

#[test]
fn a_new_session_can_pick_a_kind_from_its_token() {
    for kind in [K::AllowAny, K::AllowHosts, K::Block, K::DirectoryRule] {
        assert_eq!(kind_from_token(kind_token(kind)), Some(kind));
    }
    assert_eq!(kind_from_token("none"), None);
    assert_eq!(kind_from_token(""), None);
}
