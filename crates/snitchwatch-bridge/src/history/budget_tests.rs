//! S2/S3 arithmetic of the full-journal fallback, recomputed independently of
//! `budget.rs`'s own expressions where possible.

use super::budget::*;
use super::settings::SETTINGS_PEAK_RESERVE_BYTES;

const MIB: u64 = 1024 * 1024;
const SIZES: [u64; 3] = [16 * MIB, 64 * MIB, 256 * MIB];

#[test]
fn the_main_file_is_at_most_half_of_b() {
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        assert!(budget.max_page_count * 4096 <= b / 2, "{b}");
        assert_eq!(budget.main_bytes, budget.max_page_count * 4096);
        // And not much less: the journal term costs about 0.2%.
        assert!(budget.main_bytes * 100 >= b / 2 * 99, "{b}");
    }
}

#[test]
fn main_plus_a_full_journal_plus_settings_never_exceeds_b() {
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        let full_journal = 4096 + budget.max_page_count * (4 + 4096 + 4);
        assert_eq!(budget.journal_limit_bytes(), full_journal);
        let total = budget.max_page_count * 4096 + full_journal + SETTINGS_PEAK_RESERVE_BYTES;
        assert!(total <= b, "{b}: {total}");
        assert_eq!(budget.worst_case_bytes(), total);
        assert_eq!(total + budget.slack_bytes, b);
    }
}

#[test]
fn one_more_page_would_break_the_bound() {
    // The cap is as large as the two limits allow, not just small enough.
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        let p = budget.max_page_count + 1;
        let over_half = p * 4096 > b / 2;
        let over_b = p * 4096 + 4096 + p * 4104 + SETTINGS_PEAK_RESERVE_BYTES > b;
        assert!(over_half || over_b, "{b}");
    }
}

#[test]
fn a_budget_too_small_for_a_batch_is_refused() {
    assert!(Budget::new(MIB).is_err());
    assert!(Budget::new(0).is_err());
}

#[test]
fn batch_sizes_are_constants_not_derived_from_the_budget() {
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        assert_eq!(budget.rows_per_txn, ROWS_PER_TXN);
        assert_eq!(budget.vacuum_steps_per_txn, VACUUM_STEPS_PER_TXN);
        assert!(budget.high_water_pages() < budget.max_page_count);
    }
}
