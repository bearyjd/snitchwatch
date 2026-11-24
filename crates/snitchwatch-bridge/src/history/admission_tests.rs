//! D5 admission: rates, byte share, bounded bookkeeping. The clock is a
//! plain argument; nothing here sleeps.

use super::*;

const MIB: u64 = 1024 * 1024;

fn cfg() -> AdmissionConfig {
    AdmissionConfig::standard()
}

/// Rates that never refuse, to test the bookkeeping alone.
fn unlimited() -> AdmissionConfig {
    AdmissionConfig {
        process_rate: 1_000_000,
        process_burst: 1_000_000,
        global_rate: 1_000_000,
        global_burst: 1_000_000,
        ..cfg()
    }
}

fn path(i: usize) -> String {
    format!("/usr/bin/proc-{i}")
}

#[test]
fn the_defaults_are_the_owners_numbers() {
    let c = cfg();
    assert_eq!((c.global_rate, c.process_rate), (200, 20));
    assert_eq!((c.global_burst, c.process_burst), (1000, 100));
    assert_eq!((c.max_buckets, c.idle_ms), (1024, 60_000));
}

#[test]
fn one_process_gets_its_burst_then_its_rate() {
    let mut a = Admission::new(cfg());
    let p = Some("/usr/bin/curl");
    let admitted = (0..300).filter(|_| a.admit(p, 100, 0).is_ok()).count();
    assert_eq!(admitted, 100, "burst of 100 at one instant");
    assert_eq!(a.admit(p, 100, 0), Err(Refusal::ProcessRate));
    // One second later 20 more tokens.
    let later = (0..300).filter(|_| a.admit(p, 100, 1000).is_ok()).count();
    assert_eq!(later, 20);
}

#[test]
fn a_flooder_cannot_starve_another_process() {
    let mut a = Admission::new(cfg());
    let mut now = 0;
    // The flooder: 5,000 events a second for ten seconds.
    for _ in 0..10 {
        for _ in 0..5000 {
            let _ = a.admit(Some("/tmp/flood"), 100, now);
        }
        now += 1000;
        // A well-behaved process sends two events a second.
        assert_eq!(a.admit(Some("/usr/bin/firefox"), 100, now), Ok(()));
        assert_eq!(a.admit(Some("/usr/bin/firefox"), 100, now), Ok(()));
    }
}

#[test]
fn a_process_refused_by_its_own_bucket_spends_no_global_tokens() {
    let mut a = Admission::new(cfg());
    // 100 admitted by the burst, then 100,000 refusals by the process bucket.
    for _ in 0..100_100 {
        let _ = a.admit(Some("/tmp/flood"), 100, 0);
    }
    // The global bucket (1,000) lost only the 100 the flooder was given.
    let others = (0..2000)
        .filter(|i| a.admit(Some(&path(*i)), 100, 0).is_ok())
        .count();
    assert_eq!(others, 900);
}

#[test]
fn the_global_rate_caps_many_polite_processes() {
    let mut a = Admission::new(cfg());
    let admitted = (0..2000)
        .filter(|i| a.admit(Some(&path(*i)), 100, 0).is_ok())
        .count();
    assert_eq!(admitted, 1000, "the global burst");
    assert_eq!(
        a.admit(Some("/usr/bin/x"), 100, 0),
        Err(Refusal::GlobalRate)
    );
    let next = (0..2000)
        .filter(|i| a.admit(Some(&path(*i + 5000)), 100, 1000).is_ok())
        .count();
    assert_eq!(next, 200, "200 a second sustained");
}

#[test]
fn a_byte_flood_is_not_refused_at_admission_and_the_ledger_follows_it() {
    // The share is enforced by self-eviction (PR 8, owner decision 2026-11-24,
    // item 1), so a process is never refused for the bytes it holds.
    let mut a = Admission::new(unlimited());
    let big = 4600; // a worst-case row
    for _ in 0..10_000 {
        assert_eq!(a.admit(Some("/tmp/flood"), big, 0), Ok(()));
    }
    assert_eq!(a.retained_for(Some("/tmp/flood")), 10_000 * big);
    assert_eq!(a.admit(Some("/usr/bin/firefox"), 200, 0), Ok(()));
    // Retention releases what it deletes.
    a.release_bytes(Some("/tmp/flood"), 2 * MIB);
    assert_eq!(a.retained_for(Some("/tmp/flood")), 10_000 * big - 2 * MIB);
}

#[test]
fn a_refused_record_adds_no_bytes() {
    let mut a = Admission::new(AdmissionConfig {
        process_burst: 1,
        ..cfg()
    });
    assert_eq!(a.admit(Some("p"), 900, 0), Ok(()));
    assert_eq!(a.admit(Some("p"), 200, 0), Err(Refusal::ProcessRate));
    assert_eq!(a.retained_for(Some("p")), 900);
}

#[test]
fn the_byte_ledger_is_capped_and_keeps_the_biggest_entries() {
    let mut a = Admission::new(AdmissionConfig {
        process_rate: 1_000_000,
        process_burst: 1_000_000,
        global_rate: 1_000_000,
        global_burst: 1_000_000,
        ..cfg()
    });
    a.admit(Some("/tmp/big"), 3 * MIB, 0).unwrap();
    for i in 0..5000 {
        let _ = a.admit(Some(&path(i)), 10, 0);
        assert!(a.byte_entries() <= 1024);
    }
    assert_eq!(a.retained_for(Some("/tmp/big")), 3 * MIB);
}

#[test]
fn a_clock_that_steps_back_credits_nothing_and_then_resumes_from_the_new_time() {
    let mut a = Admission::new(cfg());
    for _ in 0..100 {
        a.admit(Some("p"), 10, 10_000).unwrap();
    }
    // An hour back: no refill, and no panic.
    assert_eq!(a.admit(Some("p"), 10, 5_000), Err(Refusal::ProcessRate));
    // One second after the step, the usual 20 tokens: not stalled until
    // 10_000 comes round again, and nothing credited twice.
    let n = (0..100)
        .filter(|_| a.admit(Some("p"), 10, 6_000).is_ok())
        .count();
    assert_eq!(n, 20);
}

#[test]
fn a_missing_path_has_one_shared_bucket() {
    let mut a = Admission::new(cfg());
    let n = (0..300).filter(|_| a.admit(None, 10, 0).is_ok()).count();
    assert_eq!(n, 100);
}
