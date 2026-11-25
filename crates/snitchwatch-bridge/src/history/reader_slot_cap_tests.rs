//! One response slot holds at most one page, ever: a slot that was taken
//! cannot be filled again, so retaining the taken pages cannot exceed the
//! slot count (issue #433).

use std::time::Duration;

use super::reader_registry::*;

const DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn a_slot_that_was_taken_cannot_be_filled_again() {
    let slots = ResponseSlots::<Vec<u8>>::with_deadline(1, DEADLINE);
    let slot = slots.acquire().unwrap();
    assert!(slot.fill(vec![1]).is_ok());
    let first = slot.take().expect("the page");
    assert_eq!(
        slot.fill(vec![2]),
        Err(vec![2]),
        "a second page in the slot"
    );
    assert!(slot.take().is_none(), "nothing is left to take");
    drop(first);
}

#[test]
fn fill_take_retain_repeat_never_exceeds_the_slot_cap() {
    let cap = 2;
    let slots = ResponseSlots::<Vec<u8>>::with_deadline(cap, DEADLINE);
    let slot = slots.acquire().unwrap();
    let mut retained = Vec::new();
    for round in 0..10u8 {
        if slot.fill(vec![round]).is_ok() {
            retained.extend(slot.take());
        }
    }
    assert!(
        retained.len() <= cap,
        "{} live pages for {cap} slots",
        retained.len()
    );
    assert_eq!(retained.len(), 1, "one slot, one page");
}
