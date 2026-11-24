//! L1: the ledger is bounded in rows and bytes, collapses instead of
//! dropping, and never loses a gap silently.

use rusqlite::Connection;

use super::super::budget::Budget;
use super::super::schema::{schema_sql, GAP_CAUSES};
use super::super::session::{end_session, read_sessions, start_session, SessionEnd};
use super::*;

fn ledger_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&schema_sql()).unwrap();
    conn
}

fn small(rows: u64) -> LedgerLimits {
    LedgerLimits {
        max_rows: rows,
        max_bytes: rows * ROW_BYTES,
    }
}

fn gap(start: i64, end: i64, cause: GapCause, count: u64) -> Gap {
    Gap {
        start_ms: start,
        end_ms: end,
        start_exact: true,
        cause,
        count,
    }
}

fn covered(gaps: &[Gap], start: i64, end: i64) -> bool {
    gaps.iter().any(|g| g.start_ms <= start && g.end_ms >= end)
}

#[test]
fn every_cause_round_trips_and_the_schema_lists_exactly_them() {
    assert_eq!(ALL_CAUSES.len(), GAP_CAUSES.len());
    let conn = ledger_db();
    for (i, cause) in ALL_CAUSES.into_iter().enumerate() {
        assert_eq!(GapCause::parse(cause.as_str()), Some(cause));
        assert_eq!(cause as usize, i, "enum order is the schema order");
        record_gap(
            &conn,
            &gap(i as i64 * 10, i as i64 * 10 + 1, cause, 1),
            &small(64),
        )
        .unwrap();
    }
    assert_eq!(GapCause::parse("nonsense"), None);
    assert_eq!(read_gaps(&conn).unwrap().len(), ALL_CAUSES.len());
}

#[test]
fn a_row_never_costs_more_than_row_bytes() {
    let conn = ledger_db();
    let before: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    let worst = Gap {
        start_ms: i64::MAX - 1,
        end_ms: i64::MAX,
        start_exact: false,
        cause: GapCause::DaemonDisconnected,
        count: u64::MAX,
    };
    let rows = 4000u64;
    for i in 0..rows {
        // Distinct, non-touching, so none merges.
        let g = Gap {
            start_ms: i64::MAX - 1 - (i as i64) * 4,
            end_ms: i64::MAX - (i as i64) * 4,
            ..worst
        };
        record_gap(&conn, &g, &small(1_000_000)).unwrap();
    }
    let after: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    let per_row = (after - before) as u64 * 4096 / rows;
    assert!(per_row <= ROW_BYTES, "{per_row} bytes per row");
}

#[test]
fn the_ceiling_is_the_smaller_of_the_row_and_the_byte_limit() {
    let l = LedgerLimits {
        max_rows: 1000,
        max_bytes: 100 * ROW_BYTES,
    };
    assert_eq!(l.ceiling(), 100);
    let l = LedgerLimits {
        max_rows: 50,
        max_bytes: 100 * ROW_BYTES,
    };
    assert_eq!(l.ceiling(), 50);
    let l = LedgerLimits {
        max_rows: 1,
        max_bytes: 0,
    };
    assert_eq!(
        l.ceiling(),
        MIN_CEILING,
        "never too small to hold a collapse"
    );
    let budget = Budget::new(16 * 1024 * 1024).unwrap();
    let from = LedgerLimits::from_budget(&budget);
    assert_eq!(from.max_bytes, budget.ledger_pages * 4096);
    assert!(from.ceiling() <= DEFAULT_MAX_ROWS);
}

#[test]
fn adjacent_gaps_of_the_same_cause_merge_on_write() {
    let conn = ledger_db();
    let l = small(64);
    record_gap(&conn, &gap(10, 20, GapCause::QueueOverflow, 2), &l).unwrap();
    record_gap(&conn, &gap(20, 30, GapCause::QueueOverflow, 3), &l).unwrap();
    assert_eq!(
        read_gaps(&conn).unwrap(),
        vec![gap(10, 30, GapCause::QueueOverflow, 5)]
    );
}

#[test]
fn gaps_that_differ_in_cause_exactness_or_position_do_not_merge() {
    let conn = ledger_db();
    let l = small(64);
    record_gap(&conn, &gap(10, 20, GapCause::QueueOverflow, 1), &l).unwrap();
    record_gap(&conn, &gap(20, 30, GapCause::AdmissionDrop, 1), &l).unwrap();
    record_gap(&conn, &gap(30, 40, GapCause::AdmissionDrop, 1), &l).unwrap();
    let mut inexact = gap(40, 50, GapCause::AdmissionDrop, 1);
    inexact.start_exact = false;
    record_gap(&conn, &inexact, &l).unwrap();
    record_gap(&conn, &gap(60, 70, GapCause::AdmissionDrop, 1), &l).unwrap();
    assert_eq!(read_gaps(&conn).unwrap().len(), 4);
}

/// One restart: the session runs `[start, start + 3]`, stops cleanly, and the
/// next one starts 10 ms after this one did.
fn restart(conn: &Connection, l: &LedgerLimits, i: i64) {
    let start = i * 10;
    start_session(conn, l, start, start).unwrap();
    end_session(conn, start + 3, SessionEnd::Clean).unwrap();
}

#[test]
fn thousands_of_non_mergeable_restarts_stay_within_limits_and_collapse_to_one_uncertain_span() {
    let conn = ledger_db();
    let l = small(64);
    let restarts = 5000i64;
    for i in 0..restarts {
        restart(&conn, &l, i);
        let rows = row_count(&conn).unwrap();
        assert!(rows <= l.ceiling(), "{rows} rows after restart {i}");
        assert!(
            rows * ROW_BYTES <= l.max_bytes,
            "bytes over after restart {i}"
        );
    }
    start_session(&conn, &l, restarts * 10, restarts * 10).unwrap();
    let gaps = read_gaps(&conn).unwrap();
    let uncertain: Vec<&Gap> = gaps
        .iter()
        .filter(|g| g.cause == GapCause::Uncertain)
        .collect();
    assert_eq!(uncertain.len(), 1, "one explicit uncertain span");
    assert!(uncertain[0].is_uncertain() && !uncertain[0].start_exact);
    assert!(uncertain[0].count > 1000, "it says how many it stands for");
    // Never a silent loss: every restart's gap is covered, and counted.
    for i in 0..restarts {
        assert!(covered(&gaps, i * 10 + 3, (i + 1) * 10), "restart {i} lost");
    }
    assert_eq!(gaps.iter().map(|g| g.count).sum::<u64>(), restarts as u64);
    assert!(gaps.len() > 4, "recent restarts keep their detail");
}

#[test]
fn at_the_limit_a_new_gap_collapses_the_oldest_and_drops_nothing() {
    let conn = ledger_db();
    let l = small(MIN_CEILING * 2);
    let mut all = Vec::new();
    for i in 0..200i64 {
        let g = gap(
            i * 100,
            i * 100 + 10,
            GapCause::QueueOverflow,
            1 + i as u64 % 3,
        );
        all.push(g);
        record_gap(&conn, &g, &l).unwrap();
        assert!(row_count(&conn).unwrap() <= l.ceiling());
    }
    let kept = read_gaps(&conn).unwrap();
    for g in &all {
        assert!(covered(&kept, g.start_ms, g.end_ms), "{g:?} was dropped");
    }
    assert_eq!(
        kept.iter().map(|g| g.count).sum::<u64>(),
        all.iter().map(|g| g.count).sum::<u64>(),
        "counts are conserved"
    );
}

#[test]
fn a_collapse_never_deletes_the_open_session() {
    let conn = ledger_db();
    let l = small(MIN_CEILING);
    for i in 0..100 {
        restart(&conn, &l, i);
    }
    start_session(&conn, &l, 5000, 5000).unwrap();
    let open: Vec<_> = read_sessions(&conn)
        .unwrap()
        .into_iter()
        .filter(|s| s.end.is_none())
        .collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].start_ms, 5000);
}

#[test]
fn restarts_within_one_millisecond_merge_and_do_not_pile_up_sessions() {
    let conn = ledger_db();
    let l = small(64);
    for _ in 0..3000 {
        start_session(&conn, &l, 500, 500).unwrap();
        end_session(&conn, 500, SessionEnd::Clean).unwrap();
    }
    assert!(row_count(&conn).unwrap() <= l.ceiling());
    let gaps = read_gaps(&conn).unwrap();
    assert_eq!(gaps.iter().map(|g| g.count).sum::<u64>(), 2999);
}

#[test]
fn a_second_collapse_extends_the_one_span_instead_of_adding_another() {
    let conn = ledger_db();
    let l = small(MIN_CEILING);
    for i in 0..2000 {
        restart(&conn, &l, i);
    }
    let gaps = read_gaps(&conn).unwrap();
    assert_eq!(
        gaps.iter()
            .filter(|g| g.cause == GapCause::Uncertain)
            .count(),
        1
    );
    let u = gaps
        .iter()
        .find(|g| g.cause == GapCause::Uncertain)
        .unwrap();
    assert_eq!(u.start_ms, 3, "it begins where the oldest gap did");
}

#[test]
fn a_late_gap_never_deletes_the_newest_closed_session() {
    // The next start reads the newest session to find where the gap begins.
    let conn = ledger_db();
    let l = small(64);
    start_session(&conn, &l, 500, 500).unwrap();
    end_session(&conn, 500, SessionEnd::Clean).unwrap();
    record_gap(&conn, &gap(400, 500, GapCause::QueueOverflow, 1), &l).unwrap();
    record_gap(&conn, &gap(500, 600, GapCause::QueueOverflow, 1), &l).unwrap();
    let sessions = read_sessions(&conn).unwrap();
    assert_eq!(sessions.len(), 1, "the closed session is still there");
    let gap = start_session(&conn, &l, 700, 700).unwrap();
    assert_eq!(gap.map(|g| (g.start_ms, g.end_ms)), Some((500, 700)));
}

#[test]
fn boots_whose_clock_steps_backwards_never_lock_the_ledger() {
    let conn = ledger_db();
    let l = small(64);
    let boots = 200i64;
    // Every boot starts at 100 (an RTC-less board) and stops cleanly a long
    // time later, so no closed session lies inside any gap's time span.
    for i in 0..boots {
        start_session(&conn, &l, 100, 100).unwrap();
        end_session(&conn, 1_000_000 + i, SessionEnd::Clean).unwrap();
        let rows = row_count(&conn).unwrap();
        assert!(rows <= l.ceiling(), "{rows} rows after boot {i}");
    }
    start_session(&conn, &l, 10_000_000, 10_000_000).unwrap();
    assert!(row_count(&conn).unwrap() <= l.ceiling());
    let gaps = read_gaps(&conn).unwrap();
    // Every restart gap is covered, and the sessions that were absorbed are
    // inside the uncertain span.
    for i in 1..=boots {
        assert!(covered(&gaps, 1_000_000 + i - 1, 1_000_000 + i - 1), "{i}");
    }
    let uncertain: Vec<&Gap> = gaps
        .iter()
        .filter(|g| g.cause == GapCause::Uncertain)
        .collect();
    assert_eq!(uncertain.len(), 1);
    assert!(uncertain[0].start_ms <= 100, "covers the absorbed sessions");
    assert_eq!(gaps.iter().map(|g| g.count).sum::<u64>(), boots as u64);
}

#[test]
fn a_session_between_two_coalesced_clock_jumps_survives_the_merge() {
    let conn = ledger_db();
    let l = small(64);
    record_gap(&conn, &gap(100_000, 110_000, GapCause::ClockJump, 1), &l).unwrap();
    // A closed session inside what a non-adjacent merge would span, then a
    // different gap, so the first jump is no longer the latest row.
    conn.execute(
        "INSERT INTO sessions (start_ms, end_ms, end_kind) VALUES (120_000, 130_000, 'clean')",
        [],
    )
    .unwrap();
    record_gap(&conn, &gap(140_000, 141_000, GapCause::QueueOverflow, 1), &l).unwrap();
    conn.execute(
        "INSERT INTO sessions (start_ms, end_ms, end_kind) VALUES (150_000, NULL, NULL)",
        [],
    )
    .unwrap();
    record_gap(&conn, &gap(160_000, 170_000, GapCause::ClockJump, 1), &l).unwrap();
    let merged = read_gaps(&conn).unwrap();
    assert_eq!(merged.len(), 2, "{merged:?}");
    assert_eq!(merged[0], gap(100_000, 170_000, GapCause::ClockJump, 2));
    let sessions = read_sessions(&conn).unwrap();
    assert!(
        sessions.iter().any(|s| s.start_ms == 120_000),
        "the session with real coverage was deleted: {sessions:?}"
    );
}
