//! Per-group verdict counts for "last hour" and "today" (phase 0b): the
//! by-rule / by-default split, incremental insert/update/remove, window
//! rollover against an injected clock, and a reconnect's clear-and-replay.

use super::tests::row;
use super::*;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
/// 10:00 local on some day; `DAY` is that day's local midnight.
const T0: i64 = 1_760_000_000_000;
const DAY: i64 = T0 - 10 * HOUR;

fn clock(now_ms: i64) -> Clock {
    Clock {
        now_ms,
        today_start_ms: DAY,
    }
}

fn at(mut r: ConnectionRow, started_at_ms: i64, rule: Option<&str>) -> ConnectionRow {
    r.started_at_ms = started_at_ms;
    r.matched_rule = rule.map(str::to_string);
    r
}

fn tree_at(now_ms: i64) -> GroupTree {
    let mut tree = GroupTree::new();
    tree.set_clock(clock(now_ms));
    tree
}

fn projection(tree: &GroupTree, rows: &[ConnectionRow]) -> Vec<VisibleEntry> {
    let map: HashMap<&str, &ConnectionRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    tree.build_host_projection(&map, &ConnectionFilter::default())
}

fn process_windows(proj: &[VisibleEntry], key: &str) -> WindowFigures {
    proj.iter()
        .find_map(|e| match e {
            VisibleEntry::ProcessHeader { key: k, counts, .. } if k == key => Some(counts.windows),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no process header {key}"))
}

fn hour(tree: &GroupTree, rows: &[ConnectionRow], process: &str) -> WindowCounts {
    process_windows(&projection(tree, rows), process).last_hour
}

#[test]
fn split_separates_rule_default_and_every_other_verdict() {
    let mut tree = tree_at(T0);
    let mut by_default_flag = at(row("c", "p", "a.example", "", Some("allow")), T0, Some("r"));
    by_default_flag.decided_by_default = true;
    let mut deferred = at(row("h", "p", "a.example", "", None), T0, None);
    deferred.deferred = true;
    let rows = vec![
        at(
            row("a", "p", "a.example", "", Some("allow")),
            T0,
            Some("rule-a"),
        ),
        at(row("b", "p", "a.example", "", Some("allow")), T0, None),
        by_default_flag,
        at(
            row("d", "p", "a.example", "", Some("deny")),
            T0,
            Some("rule-d"),
        ),
        at(row("e", "p", "a.example", "", Some("deny")), T0, Some("")),
        at(
            row("f", "p", "a.example", "", Some("blocklist")),
            T0,
            Some("list"),
        ),
        at(row("g", "p", "a.example", "", None), T0, None),
        deferred,
    ];
    for r in &rows {
        tree.upsert_row(r);
    }
    let w = hour(&tree, &rows, "p");
    assert_eq!(
        w,
        WindowCounts {
            allowed_by_rule: 1,
            allowed_by_default: 2,
            denied_by_rule: 1,
            denied_by_default: 1,
            blocklisted: 1,
            pending: 1,
            other: 1,
        }
    );
    assert_eq!(process_windows(&projection(&tree, &rows), "p").today, w);
}

#[test]
fn counts_follow_insert_update_and_remove_at_every_level() {
    let mut tree = tree_at(T0);
    tree.set_process_expanded("p", true);
    tree.set_domain_expanded("p", "example.com", true);
    let pending = at(row("r1", "p", "a.example.com", "", None), T0, None);
    tree.upsert_row(&pending);
    let rows = vec![pending.clone()];
    assert_eq!(hour(&tree, &rows, "p").pending, 1);

    let decided = at(
        row("r1", "p", "a.example.com", "", Some("allow")),
        T0,
        Some("r"),
    );
    tree.upsert_row(&decided);
    let rows = vec![decided.clone()];
    let proj = projection(&tree, &rows);
    for entry in &proj {
        let windows = match entry {
            VisibleEntry::ProcessHeader { counts, .. }
            | VisibleEntry::DomainHeader { counts, .. }
            | VisibleEntry::HostHeader { counts, .. } => counts.windows,
            VisibleEntry::Row { .. } => continue,
        };
        assert_eq!(windows.last_hour.pending, 0, "{entry:?}");
        assert_eq!(windows.last_hour.allowed_by_rule, 1, "{entry:?}");
        assert_eq!(windows.today.allowed_by_rule, 1, "{entry:?}");
    }

    tree.upsert_row(&at(
        row("r2", "p", "a.example.com", "", Some("deny")),
        T0,
        None,
    ));
    tree.remove_row("r2");
    tree.remove_row("r1");
    tree.upsert_row(&at(row("r3", "p", "b.example.com", "", None), T0, None));
    let rows = vec![at(row("r3", "p", "b.example.com", "", None), T0, None)];
    let w = hour(&tree, &rows, "p");
    assert_eq!(
        (w.pending, w.allowed_by_rule, w.denied_by_default),
        (1, 0, 0)
    );
}

#[test]
fn a_row_moving_to_another_group_moves_its_figures() {
    let mut tree = tree_at(T0);
    tree.upsert_row(&at(row("r", "p", "a.example", "", None), T0, None));
    let moved = at(row("r", "q", "a.example", "", None), T0, None);
    tree.upsert_row(&moved);
    let proj = projection(&tree, std::slice::from_ref(&moved));
    assert_eq!(proj.len(), 1, "p's group is gone: {proj:?}");
    assert_eq!(process_windows(&proj, "q").last_hour.pending, 1);
}

#[test]
fn the_hour_rolls_on_the_tick_and_only_moved_headers_change() {
    let mut tree = tree_at(T0);
    let old = at(
        row("old", "p", "a.example", "", Some("allow")),
        T0,
        Some("r"),
    );
    let new = at(
        row("new", "q", "b.example", "", Some("deny")),
        T0 + 30 * MIN,
        None,
    );
    tree.upsert_row(&old);
    tree.set_clock(clock(T0 + 30 * MIN));
    tree.upsert_row(&new);
    let rows = vec![old, new];
    let mut proj = projection(&tree, &rows);
    assert_eq!(process_windows(&proj, "p").last_hour.allowed_by_rule, 1);

    tree.set_clock(clock(T0 + 45 * MIN));
    assert!(tree.refresh_window_figures(&mut proj).is_empty());

    tree.set_clock(clock(T0 + 61 * MIN));
    let changed = tree.refresh_window_figures(&mut proj);
    let p_index = proj
        .iter()
        .position(|e| matches!(e, VisibleEntry::ProcessHeader { key, .. } if key == "p"))
        .unwrap();
    assert_eq!(changed, vec![p_index], "only p's header moved");
    let p = process_windows(&proj, "p");
    assert_eq!(p.last_hour, WindowCounts::default());
    assert_eq!(p.today.allowed_by_rule, 1, "still today");
    assert_eq!(process_windows(&proj, "q").last_hour.denied_by_default, 1);
    assert_eq!(proj, projection(&tree, &rows), "tick agrees with a rebuild");
}

#[test]
fn today_starts_again_at_local_midnight() {
    let late = DAY + 24 * HOUR - 10 * MIN;
    let mut tree = GroupTree::new();
    tree.set_clock(Clock {
        now_ms: late,
        today_start_ms: DAY,
    });
    let rows = vec![at(row("r", "p", "a.example", "", None), late, None)];
    tree.upsert_row(&rows[0]);
    let mut proj = projection(&tree, &rows);
    assert_eq!(process_windows(&proj, "p").today.pending, 1);

    tree.set_clock(Clock {
        now_ms: DAY + 24 * HOUR + 5 * MIN,
        today_start_ms: DAY + 24 * HOUR,
    });
    assert_eq!(tree.refresh_window_figures(&mut proj).len(), 1);
    let w = process_windows(&proj, "p");
    assert_eq!(w.today, WindowCounts::default());
    assert_eq!(
        w.last_hour.pending, 1,
        "15 minutes ago is still the last hour"
    );

    let after = at(
        row("s", "p", "a.example", "", None),
        DAY + 24 * HOUR + 5 * MIN,
        None,
    );
    tree.upsert_row(&after);
    // Arriving after midnight does not make yesterday's start count today.
    let before_midnight = at(row("y", "p", "a.example", "", None), late + 5 * MIN, None);
    tree.upsert_row(&before_midnight);
    let shown = [rows[0].clone(), after.clone(), before_midnight.clone()];
    let w = process_windows(&projection(&tree, &shown), "p");
    assert_eq!((w.last_hour.pending, w.today.pending), (3, 1));
    // Removing yesterday's row leaves today's figure alone.
    tree.remove_row("r");
    let w = process_windows(&projection(&tree, &[after, before_midnight]), "p");
    assert_eq!((w.last_hour.pending, w.today.pending), (2, 1));
}

#[test]
fn removing_a_row_after_its_window_passed_leaves_newer_rows_counted() {
    let mut tree = tree_at(T0);
    let old = at(row("old", "p", "a.example", "", Some("allow")), T0, None);
    tree.upsert_row(&old);
    tree.set_clock(clock(T0 + 90 * MIN));
    let new = at(
        row("new", "p", "a.example", "", Some("allow")),
        T0 + 90 * MIN,
        None,
    );
    tree.upsert_row(&new);
    tree.remove_row("old");
    let rows = vec![new];
    let proj = projection(&tree, &rows);
    let w = process_windows(&proj, "p");
    assert_eq!(w.last_hour.allowed_by_default, 1);
    assert_eq!(w.today.allowed_by_default, 1);
    tree.remove_row("new");
    tree.upsert_row(&rows[0]);
    assert_eq!(hour(&tree, &rows, "p").allowed_by_default, 1);
}

#[test]
fn a_reconnect_replay_after_clear_counts_each_row_once() {
    let mut tree = tree_at(T0);
    let rows: Vec<ConnectionRow> = (0..5)
        .map(|i| {
            at(
                row(&format!("r{i}"), "p", "a.example", "", Some("deny")),
                T0 - i * MIN,
                None,
            )
        })
        .collect();
    for r in &rows {
        tree.upsert_row(r);
    }
    let before = projection(&tree, &rows);
    tree.clear();
    assert!(projection(&tree, &rows).is_empty());
    for r in &rows {
        tree.upsert_row(r);
    }
    let after = projection(&tree, &rows);
    assert_eq!(after, before);
    assert_eq!(process_windows(&after, "p").last_hour.denied_by_default, 5);
}

#[test]
fn window_text_names_every_verdict_in_words() {
    let w = WindowCounts {
        allowed_by_rule: 9,
        allowed_by_default: 3,
        denied_by_rule: 2,
        pending: 1,
        ..Default::default()
    };
    assert_eq!(
        w.summary("Last hour"),
        "Last hour: 12 allowed (3 by default) · 2 denied · 1 pending"
    );
    let all = WindowCounts {
        denied_by_default: 1,
        blocklisted: 4,
        other: 1,
        ..Default::default()
    };
    assert_eq!(
        all.segments(),
        vec![
            ("denied", "1 denied (1 by default)".to_string()),
            ("blocklisted", "4 blocklisted".to_string()),
            ("other", "1 decided another way".to_string()),
        ]
    );
    assert_eq!(
        WindowCounts::default().summary("Today"),
        "Today: no connections"
    );
}

// --- clock skew, rollback and day-boundary changes (#228 review) -----

fn replayed(clock: Clock, rows: &[ConnectionRow]) -> Vec<VisibleEntry> {
    let mut tree = GroupTree::new();
    tree.set_clock(clock);
    for r in rows {
        tree.upsert_row(r);
    }
    projection(&tree, rows)
}

#[test]
fn a_future_start_counts_only_once_its_minute_comes() {
    let mut tree = tree_at(T0);
    let rows = vec![at(
        row("f", "p", "a.example", "", None),
        T0 + 2 * HOUR,
        None,
    )];
    tree.upsert_row(&rows[0]);
    let mut proj = projection(&tree, &rows);
    assert_eq!(process_windows(&proj, "p"), WindowFigures::default());

    tree.set_clock(clock(T0 + 2 * HOUR + MIN));
    assert_eq!(tree.refresh_window_figures(&mut proj).len(), 1);
    let w = process_windows(&proj, "p");
    assert_eq!((w.last_hour.pending, w.today.pending), (1, 1));
    assert_eq!(proj, projection(&tree, &rows), "tick agrees with a rebuild");

    tree.set_clock(clock(T0 + 3 * HOUR + 2 * MIN));
    tree.refresh_window_figures(&mut proj);
    let w = process_windows(&proj, "p");
    assert_eq!((w.last_hour.pending, w.today.pending), (0, 1));
    tree.remove_row("f");
    assert!(projection(&tree, &[]).is_empty());
}

#[test]
fn a_start_skewed_past_midnight_counts_on_the_day_it_falls_on() {
    let late = DAY + 24 * HOUR - 10 * MIN;
    let mut tree = GroupTree::new();
    tree.set_clock(Clock {
        now_ms: late,
        today_start_ms: DAY,
    });
    let rows = vec![at(
        row("s", "p", "a.example", "", None),
        DAY + 24 * HOUR + 2 * MIN,
        None,
    )];
    tree.upsert_row(&rows[0]);
    let mut proj = projection(&tree, &rows);
    assert_eq!(process_windows(&proj, "p"), WindowFigures::default());

    let next = Clock {
        now_ms: DAY + 24 * HOUR + 5 * MIN,
        today_start_ms: DAY + 24 * HOUR,
    };
    tree.set_clock(next);
    tree.refresh_window_figures(&mut proj);
    let w = process_windows(&proj, "p");
    assert_eq!((w.last_hour.pending, w.today.pending), (1, 1));
    assert_eq!(proj, replayed(next, &rows));
}

#[test]
fn far_future_starts_keep_the_buckets_bounded_and_still_count_later() {
    let mut tree = tree_at(T0);
    let step = 30 * MIN;
    let rows: Vec<ConnectionRow> = (1..=2000)
        .map(|k| {
            at(
                row(&format!("f{k}"), "p", "a.example", "", None),
                T0 + k * step,
                None,
            )
        })
        .collect();
    for r in &rows {
        tree.upsert_row(r);
    }
    let bound = (windows::HOUR_MINUTES + windows::FUTURE_MINUTES) as usize;
    assert!(
        tree.max_window_buckets() <= bound,
        "{} buckets, bound {bound}",
        tree.max_window_buckets()
    );
    // Long after: f1000 started 10 minutes ago, f999 40 minutes ago.
    let later = Clock {
        now_ms: T0 + 1000 * step + 10 * MIN,
        today_start_ms: T0 + 1000 * step + 10 * MIN - 5 * HOUR,
    };
    tree.set_clock(later);
    let proj = projection(&tree, &rows);
    let w = process_windows(&proj, "p");
    assert_eq!((w.last_hour.pending, w.today.pending), (2, 10));
    assert_eq!(proj, replayed(later, &rows));
}

#[test]
fn a_clock_set_back_rebuilds_the_windows_from_the_rows() {
    let mut tree = tree_at(T0);
    let a = at(row("a", "p", "a.example", "", Some("allow")), T0, Some("r"));
    tree.upsert_row(&a);
    tree.set_clock(clock(T0 + 90 * MIN));
    let b = at(
        row("b", "p", "a.example", "", Some("deny")),
        T0 + 90 * MIN,
        None,
    );
    tree.upsert_row(&b);
    let rows = vec![a, b];
    assert_eq!(hour(&tree, &rows, "p").allowed_by_rule, 0, "A aged out");

    let back = clock(T0 + 30 * MIN);
    tree.set_clock(back);
    let mut proj = projection(&tree, &rows);
    let w = process_windows(&proj, "p");
    assert_eq!(w.last_hour.allowed_by_rule, 1, "A is back in the hour");
    assert_eq!(w.last_hour.denied_by_default, 0, "B has not started yet");
    assert_eq!(proj, replayed(back, &rows));

    // The tick path sees the same figures.
    tree.set_clock(clock(T0 + 90 * MIN));
    tree.refresh_window_figures(&mut proj);
    tree.set_clock(back);
    tree.refresh_window_figures(&mut proj);
    assert_eq!(proj, replayed(back, &rows));
}

#[test]
fn a_moved_midnight_rebuilds_today_from_the_rows() {
    let mut tree = tree_at(T0);
    let rows = vec![
        at(
            row("early", "p", "a.example", "", None),
            DAY + 30 * MIN,
            None,
        ),
        at(row("late", "p", "a.example", "", None), T0, None),
    ];
    for r in &rows {
        tree.upsert_row(r);
    }
    assert_eq!(
        process_windows(&projection(&tree, &rows), "p")
            .today
            .pending,
        2
    );
    // A time zone change moves today's start an hour later, same instant.
    let moved = Clock {
        now_ms: T0,
        today_start_ms: DAY + HOUR,
    };
    tree.set_clock(moved);
    let proj = projection(&tree, &rows);
    assert_eq!(process_windows(&proj, "p").today.pending, 1);
    assert_eq!(proj, replayed(moved, &rows));
}
