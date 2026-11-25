//! PR 10: the generation and the acceptance sequence are bridge-wide and
//! shared by every capture, and the generation starts from a per-process
//! random epoch.

use std::sync::Arc;

use super::capture::Capture;
use super::control_support::{config, event, rig, Rig};
use super::durable::tests::engine_at;
use super::guard::DecisionOutcome;
use super::lifecycle::{AcceptanceSeq, Generation};

#[test]
fn the_epoch_sits_in_the_high_bits_and_leaves_room_to_advance() {
    let g = Generation::from_epoch(5);
    assert_eq!(g.current(), 5u64 << 32);
    assert_eq!(g.advance_for_test().unwrap(), (5u64 << 32) + 1);
    assert!(Generation::random().current() >= 1u64 << 32);
}

#[test]
fn two_processes_do_not_share_a_generation_number() {
    // A cursor tagged by another process's epoch can never equal ours.
    assert_ne!(
        Generation::from_epoch(1).current() >> 32,
        Generation::from_epoch(2).current() >> 32
    );
}

#[test]
fn a_capture_uses_the_generation_and_sequence_it_is_given() {
    let Rig {
        capture,
        generation,
        acceptance,
        ..
    } = rig();
    let guard = capture.begin_decision(&super::control_support::conn("/a", "h"));
    assert_eq!(
        acceptance.highest(),
        1,
        "a prompt is numbered when accepted"
    );
    let seen = capture.shared_for_test().generation_for_test();
    assert!(
        Arc::ptr_eq(&seen, &generation),
        "one generation, not one per capture"
    );
    let events = [event("/a", "x.example", 20), event("/b", "y.example", 21)];
    capture.observe(&events.iter().collect::<Vec<_>>());
    assert_eq!(acceptance.highest(), 3, "each accepted record is numbered");
    capture.begin_ping(Default::default(), 0).finish();
    assert_eq!(acceptance.highest(), 4, "a ping is an acceptance too");
    // Settling the prompt is not a second acceptance.
    guard.unwrap().settle(DecisionOutcome::GuiLost);
    assert_eq!(acceptance.highest(), 4);
}

#[test]
fn a_respawned_capture_continues_the_same_counters() {
    let Rig {
        root: _root,
        location,
        registry,
        generation,
        acceptance,
        capture,
        ..
    } = rig();
    capture.shutdown();
    let next = Capture::spawn_shared(
        engine_at(&location),
        registry,
        config(),
        generation.clone(),
        acceptance.clone(),
    )
    .unwrap();
    let n = acceptance.highest();
    next.begin_decision(&super::control_support::conn("/a", "h"));
    assert_eq!(acceptance.highest(), n + 1);
    assert_eq!(generation.current(), 7u64 << 32);
    let _ = AcceptanceSeq::default();
}
