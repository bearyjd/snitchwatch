use super::*;
use std::sync::Arc;

#[test]
fn oldest_tracks_add_and_release_in_any_order() {
    let r = PinRegistry::new();
    assert_eq!(r.oldest(), None);
    let a = r.add(50).unwrap();
    let b = r.add(10).unwrap();
    let c = r.add(30).unwrap();
    assert_eq!(r.oldest(), Some(10));
    r.release(&a).unwrap();
    assert_eq!(r.oldest(), Some(10));
    r.release(&b).unwrap();
    assert_eq!(r.oldest(), Some(30));
    r.release(&c).unwrap();
    assert_eq!(r.oldest(), None);
    assert!(r.is_empty());
}

#[test]
fn equal_arrival_times_are_distinct_pins() {
    let r = PinRegistry::new();
    let a = r.add(5).unwrap();
    let b = r.add(5).unwrap();
    r.release(&a).unwrap();
    assert_eq!(r.oldest(), Some(5), "b still pins 5");
    r.release(&b).unwrap();
    assert_eq!(r.oldest(), None);
}

#[test]
fn double_and_unknown_release_are_errors_not_panics() {
    let r = PinRegistry::new();
    let a = r.add(1).unwrap();
    let keep = r.add(2).unwrap();
    r.release(&a).unwrap();
    assert_eq!(r.release(&a), Err(PinError::Unknown));
    assert_eq!(r.oldest(), Some(2), "the failed release changed nothing");
    r.release(&keep).unwrap();
}

#[test]
fn a_handle_from_another_registry_cannot_release_a_live_pin() {
    // Both registries issue id 0 while A's pin is still live.
    let a = PinRegistry::new();
    let b = PinRegistry::new();
    let live = a.add(10).unwrap();
    let foreign = b.add(20).unwrap();
    assert_eq!(a.release(&foreign), Err(PinError::Foreign));
    assert_eq!(a.oldest(), Some(10), "A's pin and watermark are untouched");
    assert_eq!(a.watermark(100), 10);
    assert_eq!(b.release(&live), Err(PinError::Foreign));
    assert_eq!(b.oldest(), Some(20));
    a.release(&live).unwrap();
    b.release(&foreign).unwrap();
}

#[test]
fn watermark_is_min_of_commit_and_oldest_pin() {
    let r = PinRegistry::new();
    assert_eq!(r.watermark(100), 100);
    let p = r.add(40).unwrap();
    assert_eq!(r.watermark(100), 40);
    assert_eq!(r.watermark(30), 30, "never later than the commit");
    r.release(&p).unwrap();
    assert_eq!(r.watermark(100), 100);
}

#[test]
fn negative_and_extreme_times_are_ordered() {
    let r = PinRegistry::new();
    r.add(i64::MAX).unwrap();
    r.add(i64::MIN).unwrap();
    assert_eq!(r.oldest(), Some(i64::MIN));
    assert_eq!(r.watermark(i64::MAX), i64::MIN);
}

#[test]
fn id_exhaustion_is_an_error_and_leaves_state_intact() {
    let r = PinRegistry::with_next_id(u64::MAX - 1);
    r.add(1).unwrap();
    assert_eq!(r.add(2), Err(PinError::Exhausted));
    assert_eq!(r.len(), 1);
    assert_eq!(r.oldest(), Some(1));
}

#[test]
fn concurrent_add_release_leaves_consistent_registry() {
    let r = Arc::new(PinRegistry::new());
    let keeper = r.add(-1).unwrap();
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                for i in 0..500 {
                    let id = r.add(t * 1000 + i).unwrap();
                    assert!(r.oldest().unwrap() <= t * 1000 + i);
                    r.release(&id).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(r.len(), 1);
    assert_eq!(r.oldest(), Some(-1));
    r.release(&keeper).unwrap();
    assert!(r.is_empty());
}

#[test]
fn a_poisoned_lock_is_recovered() {
    let r = Arc::new(PinRegistry::new());
    let r2 = Arc::clone(&r);
    let _ = std::thread::spawn(move || {
        let _g = r2.inner.lock().unwrap();
        panic!("poison");
    })
    .join();
    let id = r.add(3).unwrap();
    assert_eq!(r.oldest(), Some(3));
    r.release(&id).unwrap();
}
