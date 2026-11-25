//! The control gate's guard (overlap, fail closed) and the response slots'
//! memory bound at the send deadline.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::reader_registry::*;

const LONG: Duration = Duration::from_secs(10);

fn refused(registry: &ReaderRegistry) -> bool {
    registry.try_acquire().err() == Some(NotAdmitted::ControlOperation)
}

#[test]
fn overlapping_control_operations_reopen_only_when_the_last_finishes() {
    let registry = ReaderRegistry::new();
    let first = registry.quiesce(LONG).ok().unwrap();
    let second = registry.quiesce(LONG).ok().unwrap();
    drop(first);
    assert!(
        refused(&registry),
        "the second operation is still running: no reader may be admitted"
    );
    drop(second);
    assert!(registry.try_acquire().is_ok());
}

#[test]
fn quiesce_cancels_what_runs_and_admits_nothing_afterwards() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    let worker = std::thread::spawn(move || {
        let _ticket = ticket;
        while !token.is_cancelled() {
            std::thread::yield_now();
        }
    });
    let guard = registry.quiesce(LONG).ok().expect("idle once cancelled");
    worker.join().unwrap();
    assert_eq!(registry.in_use(), 0);
    assert!(refused(&registry));
    drop(guard);
}

#[test]
fn a_quiesce_that_times_out_stays_closed_even_if_the_error_is_dropped() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let outcome = registry.quiesce(Duration::from_millis(30));
    assert!(outcome.is_err(), "a stuck worker is not idle");
    drop(outcome);
    assert!(refused(&registry), "fail closed");
    drop(ticket);
}

#[test]
fn a_timed_out_quiesce_can_wait_again_or_give_up() {
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let Err(not_idle) = registry.quiesce(Duration::from_millis(20)) else {
        panic!("a stuck worker is not idle")
    };
    drop(ticket);
    let guard = not_idle.wait_again(LONG).ok().expect("idle now");
    assert!(refused(&registry));
    drop(guard);
    assert!(registry.try_acquire().is_ok());

    let ticket = registry.try_acquire().unwrap();
    let Err(not_idle) = registry.quiesce(Duration::from_millis(20)) else {
        panic!("not idle")
    };
    not_idle.give_up();
    assert!(!refused(&registry), "given up: open again");
    drop(ticket);
}

#[test]
fn waiting_again_is_bound_to_the_registry_that_timed_out() {
    let stuck_registry = ReaderRegistry::new();
    let other = ReaderRegistry::new();
    let ticket = stuck_registry.try_acquire().unwrap();
    let Err(not_idle) = stuck_registry.quiesce(Duration::from_millis(20)) else {
        panic!("a stuck worker is not idle")
    };
    // `other` is idle, and the type gives no way to ask it instead.
    assert!(other.quiesce(LONG).is_ok());
    let outcome = not_idle.wait_again(Duration::from_millis(20));
    assert!(outcome.is_err(), "the stuck registry's worker still runs");
    assert_eq!(stuck_registry.in_use(), 1, "the worker still owns its slot");
    let Err(not_idle) = outcome else {
        unreachable!()
    };
    drop(ticket);
    assert!(not_idle.wait_again(LONG).is_ok());
}

struct Counted(Arc<AtomicUsize>);
impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn wait_past(deadline: Duration) {
    std::thread::sleep(deadline * 3);
}

#[test]
fn a_page_being_sent_keeps_its_place_past_the_send_deadline() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let deadline = Duration::from_millis(20);
    let slots = ResponseSlots::<Counted>::with_deadline(1, deadline);
    let slot = slots.acquire().unwrap();
    assert!(slot.fill(Counted(Arc::clone(&dropped))).is_ok());
    let in_flight = slot.take().expect("the page");
    wait_past(deadline);
    assert!(slot.overdue(), "the sender is told to stop");
    assert!(
        slots.acquire().is_none(),
        "the page is still in memory, so its place is still counted"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(in_flight);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    drop(slot);
    assert!(slots.acquire().is_some(), "free once the page is gone");
}

#[test]
fn a_sweep_drops_an_unsent_page_without_waiting_for_a_later_acquire() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let deadline = Duration::from_millis(20);
    let slots = ResponseSlots::<Counted>::with_deadline(1, deadline);
    let slot = slots.acquire().unwrap();
    assert!(slot.fill(Counted(Arc::clone(&dropped))).is_ok());
    let give_up = Instant::now() + LONG;
    while dropped.load(Ordering::SeqCst) == 0 {
        slots.sweep();
        assert!(Instant::now() < give_up, "never swept");
        std::thread::yield_now();
    }
    assert!(slot.reclaimed());
}
