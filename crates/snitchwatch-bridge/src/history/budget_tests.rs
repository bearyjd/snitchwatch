//! S2/S3 arithmetic, recomputed independently of `budget.rs`'s own
//! expressions where possible.

use super::budget::*;
use super::caps;
use super::settings::SETTINGS_PEAK_RESERVE_BYTES;

const MIB: u64 = 1024 * 1024;
const SIZES: [u64; 3] = [16 * MIB, 64 * MIB, 256 * MIB];

#[test]
fn the_worst_case_footprint_never_exceeds_b() {
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        assert!(
            budget.worst_case_bytes() <= b,
            "{b}: {}",
            budget.worst_case_bytes()
        );
        // 10% slack is never allocated: it is left over, not just reserved.
        assert!(
            budget.max_page_count * PAGE
                + budget.journal_limit_bytes()
                + SETTINGS_PEAK_RESERVE_BYTES
                <= b - b / 10
        );
    }
}

#[test]
fn a_transaction_of_the_chosen_size_fits_the_journal_and_one_more_row_does_not() {
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        let n = budget.rows_per_txn;
        assert!(n >= 1);
        assert!(
            budget.pages_for_rows(n) + COMMIT_RESERVE_PAGES <= budget.journal_pages,
            "{b}"
        );
        assert!(
            budget.pages_for_rows(n + 1) + COMMIT_RESERVE_PAGES > budget.journal_pages,
            "{b}: not maximal"
        );
        let steps = budget.vacuum_steps_per_txn;
        assert!(
            budget.fixed_pages + steps * VACUUM_STEP_PAGES + COMMIT_RESERVE_PAGES
                <= budget.journal_pages
        );
        assert!(
            budget.fixed_pages + (steps + 1) * VACUUM_STEP_PAGES + COMMIT_RESERVE_PAGES
                > budget.journal_pages
        );
    }
}

#[test]
fn the_journal_limit_is_a_real_file_size_bound() {
    for b in SIZES {
        let budget = Budget::new(b).unwrap();
        assert_eq!(
            budget.journal_limit_bytes(),
            4096 + budget.journal_pages * (4 + 4096 + 4)
        );
        assert!(budget.journal_limit_bytes() <= budget.journal_bytes);
    }
}

#[test]
fn depth_follows_from_the_page_count_and_the_fanout() {
    // 2 * 32^(d-2) leaves are needed for depth d.
    assert_eq!(max_depth(1), 1);
    assert_eq!(max_depth(2), 2);
    assert_eq!(max_depth(63), 2);
    assert_eq!(max_depth(64), 3);
    assert_eq!(max_depth(2047), 3);
    assert_eq!(max_depth(2048), 4);
    assert_eq!(max_depth(65535), 4);
    assert_eq!(max_depth(65536), 5);
    assert_eq!(Budget::new(16 * MIB).unwrap().depth, 4);
    // The main file is about 77% of B: 256 MiB is 50,6xx pages, under 65,536.
    let big = Budget::new(256 * MIB).unwrap();
    assert!(big.max_page_count < 65536);
    assert_eq!(big.depth, 4);
}

#[test]
fn ptrmap_pages_are_one_per_820_pages() {
    assert_eq!(ptrmap_pages(1), 1);
    assert_eq!(ptrmap_pages(820), 1);
    assert_eq!(ptrmap_pages(821), 2);
    assert_eq!(ptrmap_pages(65536), 80);
}

#[test]
fn overflow_follows_sqlite_local_payload_rules() {
    assert_eq!(overflow_pages(4061), 0);
    assert_eq!(overflow_pages(4062), 1);
    // maxLocal = U - 35; minLocal = 489; each overflow page holds 4092.
    assert_eq!(overflow_pages(489 + 4092), 1);
    assert_eq!(overflow_pages(489 + 4093), 2);
    assert_eq!(overflow_pages(MAX_EVENT_PAYLOAD), 2);
    assert_eq!(overflow_pages(MAX_SNAPSHOT_PAYLOAD), 2);
}

#[test]
fn the_row_constant_is_the_documented_sum() {
    // depth 4: per tree op = 8*4 + 2 = 34; events: 3 trees + 2 overflow pages
    // * 2 (page + trunk); snapshot: 2 trees + 2 overflow pages * 2.
    let budget = Budget::new(256 * MIB).unwrap();
    assert_eq!(per_tree_op(4), 34);
    assert_eq!(budget.row_pages, (3 * 34 + 4) + (2 * 34 + 4));
    assert_eq!(budget.row_pages, 178);
    assert_eq!(budget.fixed_pages, 1 + ptrmap_pages(budget.max_page_count));
    // The deepest the budgets allow: a 65,536-page file.
    assert_eq!(per_tree_op(max_depth(65536)), 42);
}

#[test]
fn a_budget_too_small_for_one_row_is_refused() {
    assert!(Budget::new(MIB).is_err());
    assert!(Budget::new(0).is_err());
}

#[test]
fn the_cap_table_bounds_the_payload_constant() {
    let columns: usize = 8
        + 8
        + caps::PROCESS_PATH
        + caps::DST_HOST
        + caps::IP_TEXT
        + 2
        + caps::PROTOCOL
        + 18
        + 13
        + caps::RULE_NAME
        + caps::RULE_HASH
        + caps::RULE_CLASS
        + 8
        + 2
        + caps::IP_TEXT;
    assert!(MAX_EVENT_PAYLOAD as usize >= columns + 20);
}
