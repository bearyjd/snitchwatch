use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;

#[test]
fn generation_advances_monotonically() {
    let g = Generation::default();
    assert_eq!(g.current(), 0);
    assert_eq!(g.advance(), Ok(1));
    assert_eq!(g.advance(), Ok(2));
    assert_eq!(g.current(), 2);
}

#[test]
fn generation_does_not_wrap() {
    let g = Generation::new(u64::MAX - 1);
    assert_eq!(g.advance(), Ok(u64::MAX));
    assert_eq!(g.advance(), Err(Exhausted));
    assert_eq!(g.current(), u64::MAX, "stays put, never wraps to 0");
}

#[test]
fn acceptance_sequence_starts_at_one_and_is_strict() {
    let s = AcceptanceSeq::default();
    assert_eq!(s.highest(), 0);
    assert!(!s.is_above(0));
    assert_eq!(s.assign(), Ok(1));
    assert_eq!(s.assign(), Ok(2));
    assert!(s.is_above(1));
    assert!(!s.is_above(2), "equal to the boundary is not above it");
    assert!(!s.is_above(3));
}

#[test]
fn acceptance_sequence_does_not_wrap() {
    let s = AcceptanceSeq::new(u64::MAX);
    assert_eq!(s.assign(), Err(Exhausted));
    assert_eq!(s.highest(), u64::MAX);
    assert!(s.is_above(u64::MAX - 1));
}

#[test]
fn counters_are_unique_across_threads() {
    let s = Arc::new(AcceptanceSeq::default());
    let g = Arc::new(Generation::default());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (s, g) = (Arc::clone(&s), Arc::clone(&g));
            std::thread::spawn(move || {
                (0..1000)
                    .map(|_| (s.assign().unwrap(), g.advance().unwrap()))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut seqs = BTreeSet::new();
    let mut gens = BTreeSet::new();
    for h in handles {
        for (a, b) in h.join().unwrap() {
            assert!(seqs.insert(a) && gens.insert(b), "duplicate number");
        }
    }
    assert_eq!(seqs.len(), 8000);
    assert_eq!(s.highest(), 8000);
    assert_eq!(g.current(), 8000);
}

#[test]
fn gate_admits_up_to_limit_then_busy() {
    let gate = ReaderGate::new(2);
    let a = gate.try_acquire().unwrap();
    let _b = gate.try_acquire().unwrap();
    assert_eq!(gate.try_acquire().unwrap_err(), Busy);
    assert_eq!(gate.in_use(), 2, "a refusal takes nothing");
    drop(a);
    assert_eq!(gate.in_use(), 1);
    let _c = gate.try_acquire().unwrap();
    assert_eq!(gate.try_acquire().unwrap_err(), Busy);
}

#[test]
fn gate_releases_only_when_the_holder_drops() {
    let gate = ReaderGate::new(1);
    let permit = gate.try_acquire().unwrap();
    let clone = gate.clone();
    assert_eq!(clone.try_acquire().unwrap_err(), Busy, "clones share slots");
    assert_eq!(gate.in_use(), 1);
    drop(permit);
    assert_eq!(gate.in_use(), 0);
}

#[test]
fn gate_releases_when_the_holder_panics() {
    let gate = ReaderGate::new(1);
    let g2 = gate.clone();
    let res = std::thread::spawn(move || {
        let _permit = g2.try_acquire().unwrap();
        panic!("holder dies");
    })
    .join();
    assert!(res.is_err());
    assert_eq!(gate.in_use(), 0);
    assert!(gate.try_acquire().is_ok());
}

#[test]
fn gate_never_exceeds_limit_under_contention() {
    let gate = ReaderGate::new(3);
    let peak = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (gate, peak) = (gate.clone(), Arc::clone(&peak));
            std::thread::spawn(move || {
                for _ in 0..2000 {
                    if let Ok(p) = gate.try_acquire() {
                        peak.fetch_max(gate.in_use(), Ordering::SeqCst);
                        drop(p);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(peak.load(Ordering::SeqCst) <= 3);
    assert_eq!(gate.in_use(), 0);
}

#[test]
fn zero_limit_always_refuses() {
    assert_eq!(ReaderGate::new(0).try_acquire().unwrap_err(), Busy);
}
