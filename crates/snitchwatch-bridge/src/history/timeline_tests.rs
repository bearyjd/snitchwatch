use super::*;
use crate::history::ledger::GapCause;
use crate::history::session::SessionEnd;
use proptest::prelude::*;

fn win(lo: i64, hi: i64) -> Window {
    Window::new(lo, hi).unwrap()
}

fn session(start: i64, end: Option<i64>) -> Session {
    Session {
        start_ms: start,
        end_ms: end,
        end: end.map(|_| SessionEnd::Clean),
    }
}

fn gap(start: i64, end: i64, cause: GapCause) -> Gap {
    Gap {
        start_ms: start,
        end_ms: end,
        start_exact: true,
        cause,
        count: 1,
    }
}

fn spans(segments: &[Segment]) -> Vec<(i64, i64, Kind, bool)> {
    segments
        .iter()
        .map(|s| (s.start_ms, s.end_ms, s.kind, s.session))
        .collect()
}

/// The window is tiled exactly: starts at `lo`, ends at `hi`, no hole and no
/// inversion between neighbours.
fn assert_tiles(window: Window, segments: &[Segment]) {
    assert!(!segments.is_empty(), "a window is never shown as nothing");
    assert_eq!(segments[0].start_ms, window.lo_ms);
    assert_eq!(segments[segments.len() - 1].end_ms, window.hi_ms);
    for s in segments {
        assert!(s.start_ms <= s.end_ms, "inverted: {s:?}");
    }
    for pair in segments.windows(2) {
        assert_eq!(pair[0].end_ms, pair[1].start_ms, "hole between {pair:?}");
    }
}

#[test]
fn an_inverted_window_is_refused() {
    assert!(Window::new(5, 4).is_none());
    assert!(Window::new(5, 5).is_some());
}

#[test]
fn a_window_with_no_ledger_is_unexplained_never_blank() {
    let w = win(0, 100);
    let segments = coverage(w, &[], &[], None);
    assert_tiles(w, &segments);
    assert_eq!(spans(&segments), vec![(0, 100, Kind::Unexplained, false)]);
}

#[test]
fn a_running_session_is_recorded() {
    let w = win(0, 100);
    let segments = coverage(w, &[session(0, None)], &[], None);
    assert_eq!(spans(&segments), vec![(0, 100, Kind::Recorded, true)]);
}

#[test]
fn a_consistent_ledger_has_no_unexplained_stretch() {
    let w = win(0, 100);
    let sessions = [session(0, Some(50)), session(70, None)];
    let gaps = [gap(50, 70, GapCause::BridgeDown)];
    let segments = coverage(w, &sessions, &gaps, None);
    assert_tiles(w, &segments);
    assert_eq!(
        spans(&segments),
        vec![
            (0, 50, Kind::Recorded, true),
            (50, 70, Kind::Gap, false),
            (70, 100, Kind::Recorded, true),
        ]
    );
    assert_eq!(segments[1].gaps, vec![0]);
}

#[test]
fn a_hole_between_sessions_is_named_not_left_blank() {
    let w = win(0, 100);
    let sessions = [session(0, Some(40)), session(60, None)];
    let segments = coverage(w, &sessions, &[], None);
    assert_tiles(w, &segments);
    assert_eq!(
        spans(&segments),
        vec![
            (0, 40, Kind::Recorded, true),
            (40, 60, Kind::Unexplained, false),
            (60, 100, Kind::Recorded, true),
        ]
    );
}

#[test]
fn a_loss_inside_a_running_session_is_a_gap_that_was_recording() {
    let w = win(0, 100);
    let segments = coverage(
        w,
        &[session(0, None)],
        &[gap(30, 40, GapCause::UpstreamLoss)],
        None,
    );
    assert_eq!(
        spans(&segments),
        vec![
            (0, 30, Kind::Recorded, true),
            (30, 40, Kind::Gap, true),
            (40, 100, Kind::Recorded, true),
        ]
    );
}

#[test]
fn a_gap_is_never_shrunk_to_the_window() {
    let w = win(100, 200);
    let gaps = [gap(0, 1000, GapCause::ClockJump)];
    let segments = coverage(w, &[session(0, None)], &gaps, None);
    assert_tiles(w, &segments);
    assert_eq!(spans(&segments), vec![(100, 200, Kind::Gap, true)]);
    assert_eq!(segments[0].gaps, vec![0]);
}

#[test]
fn overlapping_gaps_are_both_named_and_leave_no_hole() {
    let w = win(0, 100);
    let gaps = [
        gap(10, 60, GapCause::ClockJump),
        gap(40, 90, GapCause::UpstreamLoss),
    ];
    let segments = coverage(w, &[session(0, None)], &gaps, None);
    assert_tiles(w, &segments);
    let middle = segments
        .iter()
        .find(|s| s.start_ms == 40 && s.end_ms == 60)
        .unwrap();
    assert_eq!(middle.gaps, vec![0, 1]);
    assert!(segments.iter().all(|s| s.kind != Kind::Unexplained));
}

#[test]
fn a_zero_length_gap_is_still_a_segment() {
    let w = win(0, 100);
    let gaps = [gap(50, 50, GapCause::ClockJump)];
    let segments = coverage(w, &[session(0, None)], &gaps, None);
    assert_tiles(w, &segments);
    assert!(segments
        .iter()
        .any(|s| s.start_ms == 50 && s.end_ms == 50 && s.gaps == vec![0]));
}

#[test]
fn what_lies_below_the_retention_floor_is_not_blank() {
    let w = win(0, 100);
    let segments = coverage(w, &[session(50, None)], &[], Some(50));
    assert_eq!(
        spans(&segments),
        vec![
            (0, 50, Kind::BeforeFloor, false),
            (50, 100, Kind::Recorded, true)
        ]
    );
    // A marker below the floor explains its stretch better than "before".
    let gaps = [gap(0, 20, GapCause::Evicted)];
    let segments = coverage(w, &[session(50, None)], &gaps, Some(50));
    assert_tiles(w, &segments);
    assert_eq!(segments[0].kind, Kind::Gap);
    assert_eq!(segments[1].kind, Kind::BeforeFloor);
}

#[test]
fn the_floor_splits_a_stretch_that_straddles_it() {
    let w = win(0, 100);
    let segments = coverage(w, &[session(60, None)], &[], Some(30));
    assert_tiles(w, &segments);
    assert_eq!(
        spans(&segments),
        vec![
            (0, 30, Kind::BeforeFloor, false),
            (30, 60, Kind::Unexplained, false),
            (60, 100, Kind::Recorded, true),
        ]
    );
}

#[test]
fn a_clock_stepped_back_leaves_inverted_rows_that_never_break_the_tiling() {
    let w = win(0, 100);
    let sessions = [session(80, Some(20)), session(0, Some(50))];
    let segments = coverage(w, &sessions, &[], None);
    assert_tiles(w, &segments);
    assert!(segments.iter().any(|s| s.kind == Kind::Recorded));
    assert!(segments.iter().any(|s| s.kind == Kind::Unexplained));
}

#[test]
fn a_gap_overlaps_events_when_one_lies_within_its_bounds() {
    let g = gap(10, 20, GapCause::Evicted);
    assert!(gap_overlaps_events(&g, &[5, 15, 30]));
    assert!(gap_overlaps_events(&g, &[10]));
    assert!(gap_overlaps_events(&g, &[20]));
    assert!(!gap_overlaps_events(&g, &[9, 21]));
    assert!(!gap_overlaps_events(&g, &[]));
}

proptest! {
    /// Whatever the ledger holds (inverted sessions from a stepped-back
    /// clock, gaps anywhere, any floor), the window is tiled.
    #[test]
    fn any_ledger_at_all_still_tiles(
        sessions in proptest::collection::vec((0i64..200, proptest::option::of(0i64..200)), 0..6),
        gaps in proptest::collection::vec(arbitrary_gap(), 0..6),
        floor in proptest::option::of(0i64..200),
        lo in 0i64..150,
        len in 0i64..150,
    ) {
        let sessions: Vec<Session> = sessions.into_iter().map(|(s, e)| session(s, e)).collect();
        let w = win(lo, lo + len);
        assert_tiles(w, &coverage(w, &sessions, &gaps, floor));
    }
}

fn arbitrary_gap() -> impl Strategy<Value = Gap> {
    (0i64..200, 0i64..60, 0usize..10).prop_map(|(start, len, cause)| {
        let cause = [
            GapCause::BridgeDown,
            GapCause::DaemonDisconnected,
            GapCause::UpstreamLoss,
            GapCause::QueueOverflow,
            GapCause::AdmissionDrop,
            GapCause::WriteFailed,
            GapCause::Disabled,
            GapCause::Evicted,
            GapCause::ClockJump,
            GapCause::Uncertain,
        ][cause];
        gap(start, start + len, cause)
    })
}

/// A ledger like the real one: sessions with, between each pair, a gap row
/// covering the stretch (plus some extra overlapping gaps).
fn consistent_ledger() -> impl Strategy<Value = (Vec<Session>, Vec<Gap>)> {
    (
        proptest::collection::vec((1i64..30, 1i64..30), 1..6),
        proptest::collection::vec(arbitrary_gap(), 0..6),
        proptest::bool::ANY,
    )
        .prop_map(|(steps, extra, open_last)| {
            let mut sessions = Vec::new();
            let mut gaps = Vec::new();
            let mut t = 0i64;
            for (i, (run, down)) in steps.iter().enumerate() {
                let last = i == steps.len() - 1;
                let end = t + run;
                if last && open_last {
                    sessions.push(session(t, None));
                    break;
                }
                sessions.push(session(t, Some(end)));
                if !last {
                    gaps.push(gap(end, end + down, GapCause::BridgeDown));
                    t = end + down;
                }
            }
            gaps.extend(extra);
            (sessions, gaps)
        })
}

proptest! {
    #[test]
    fn coverage_tiles_every_window_and_never_shrinks_a_gap(
        (sessions, gaps) in consistent_ledger(),
        lo in 0i64..150,
        len in 0i64..150,
    ) {
        let w = win(lo, lo + len);
        let segments = coverage(w, &sessions, &gaps, None);
        assert_tiles(w, &segments);
        // Time after the last closed session with nothing there is outside
        // any ledger: only that may be unexplained.
        let ledger_end = sessions
            .last()
            .map(|s| s.end_ms.unwrap_or(i64::MAX))
            .unwrap_or(0);
        for s in &segments {
            if s.kind == Kind::Unexplained {
                prop_assert!(s.start_ms >= ledger_end, "{s:?} in a consistent ledger");
            }
        }
        for (i, g) in gaps.iter().enumerate() {
            // Where a gap overlaps the window, its segments span the whole
            // overlap (never less than the gap's own bounds, clipped to the
            // window).
            let from = g.start_ms.max(w.lo_ms);
            let to = g.end_ms.min(w.hi_ms);
            // A gap that only touches the window at one instant has no
            // duration inside it (the reader still lists the row); a
            // zero-length gap is a point of its own and is kept.
            if from > to || (from == to && g.start_ms != g.end_ms) {
                continue;
            }
            let listed: Vec<&Segment> =
                segments.iter().filter(|s| s.gaps.contains(&i)).collect();
            prop_assert!(!listed.is_empty(), "gap {i} vanished");
            prop_assert_eq!(listed.iter().map(|s| s.start_ms).min().unwrap(), from);
            prop_assert_eq!(listed.iter().map(|s| s.end_ms).max().unwrap(), to);
            for pair in listed.windows(2) {
                prop_assert_eq!(pair[0].end_ms, pair[1].start_ms);
            }
        }
    }

    #[test]
    fn every_instant_is_named_by_something_true(
        (sessions, gaps) in consistent_ledger(),
        lo in 0i64..150,
        len in 1i64..150,
        t_off in 0i64..150,
    ) {
        let w = win(lo, lo + len);
        let t = lo + t_off % (len + 1);
        let segments = coverage(w, &sessions, &gaps, None);
        let in_session = sessions
            .iter()
            .filter(|s| s.end_ms.unwrap_or(i64::MAX) > w.lo_ms && s.start_ms < w.hi_ms)
            .any(|s| s.start_ms <= t && t <= s.end_ms.unwrap_or(i64::MAX));
        let in_gap: Vec<usize> = gaps
            .iter()
            .enumerate()
            .filter(|(_, g)| g.start_ms <= t && t <= g.end_ms)
            // Touching the window at one instant has no duration in it.
            .filter(|(_, g)| {
                g.start_ms == g.end_ms || (g.end_ms > w.lo_ms && g.start_ms < w.hi_ms)
            })
            .map(|(i, _)| i)
            .collect();
        let here: Vec<&Segment> = segments
            .iter()
            .filter(|s| s.start_ms <= t && t <= s.end_ms)
            .collect();
        prop_assert!(!here.is_empty());
        if in_session {
            prop_assert!(here.iter().any(|s| s.session), "t={t}");
        }
        for i in in_gap {
            prop_assert!(here.iter().any(|s| s.gaps.contains(&i)), "t={t} gap {i}");
        }
    }
}
