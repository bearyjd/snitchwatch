//! Connection history PR 9: what a window of time was, as a tiling.
//!
//! [`coverage`] turns a window and the ledger (sessions and gap rows) into
//! segments that tile the window exactly: the first starts at the window's
//! start, the last ends at its end, each starts where the previous one ended.
//! No instant is ever left out, so the view cannot show a hole as an empty
//! interval (learning `history-gap-rows-must-cover-every-instant`).
//!
//! Pure: no SQL, no clock. Overlap is fine (a gap inside a running session,
//! two gaps over each other); a stretch that nothing explains is the one
//! failure, and it is returned as [`Kind::Unexplained`] rather than dropped.
//! A gap is never clipped: the window only decides which part is shown.

use super::ledger::Gap;
use super::session::Session;

/// A stretch of time, inclusive at both ends (milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub lo_ms: i64,
    pub hi_ms: i64,
}

impl Window {
    /// `None` for an inverted window.
    pub fn new(lo_ms: i64, hi_ms: i64) -> Option<Self> {
        (lo_ms <= hi_ms).then_some(Self { lo_ms, hi_ms })
    }
}

/// What a stretch of the window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A session was running and nothing is known to be missing.
    Recorded,
    /// One or more gap rows cover it (see [`Segment::gaps`]). With
    /// [`Segment::session`] set, recording was running but lost items;
    /// without it, nothing was recording.
    Gap,
    /// Older than the earliest surviving event and not explained by any row:
    /// evicted by retention, or from before history was recording.
    BeforeFloor,
    /// Nothing explains it. Should not happen with a consistent ledger; it
    /// is carried instead of an empty span, never hidden.
    Unexplained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: Kind,
    /// A session covers the whole segment.
    pub session: bool,
    /// Indexes into the `gaps` slice that cover the whole segment.
    pub gaps: Vec<usize>,
}

fn session_covers(session: &Session, a: i64, b: i64) -> bool {
    session.start_ms <= a && session.end_ms.unwrap_or(i64::MAX) >= b
}

fn gap_covers(gap: &Gap, a: i64, b: i64) -> bool {
    gap.start_ms <= a && gap.end_ms >= b
}

/// Every point where something starts or ends inside the window, plus its
/// two ends, sorted and distinct.
fn cut_points(window: Window, sessions: &[Session], gaps: &[Gap]) -> Vec<i64> {
    let mut points = vec![window.lo_ms, window.hi_ms];
    let edges = sessions
        .iter()
        .flat_map(|s| [Some(s.start_ms), s.end_ms])
        .chain(gaps.iter().flat_map(|g| [Some(g.start_ms), Some(g.end_ms)]))
        .flatten();
    points.extend(edges.filter(|p| *p > window.lo_ms && *p < window.hi_ms));
    points.sort_unstable();
    points.dedup();
    points
}

fn classify(
    (a, b): (i64, i64),
    sessions: &[Session],
    gaps: &[Gap],
    floor_ms: Option<i64>,
) -> Segment {
    let session = sessions.iter().any(|s| session_covers(s, a, b));
    let covering: Vec<usize> = gaps
        .iter()
        .enumerate()
        .filter(|(_, g)| gap_covers(g, a, b))
        .map(|(i, _)| i)
        .collect();
    let kind = if !covering.is_empty() {
        Kind::Gap
    } else if session {
        Kind::Recorded
    } else if floor_ms.is_some_and(|floor| b <= floor) {
        Kind::BeforeFloor
    } else {
        Kind::Unexplained
    };
    Segment {
        start_ms: a,
        end_ms: b,
        kind,
        session,
        gaps: covering,
    }
}

fn same_state(a: &Segment, b: &Segment) -> bool {
    a.kind == b.kind && a.session == b.session && a.gaps == b.gaps
}

/// The window as a tiling. `floor_ms` is the earliest surviving event's
/// time (the retention floor), when there is one.
pub fn coverage(
    window: Window,
    sessions: &[Session],
    gaps: &[Gap],
    floor_ms: Option<i64>,
) -> Vec<Segment> {
    let points = cut_points(window, sessions, gaps);
    let mut out: Vec<Segment> = Vec::new();
    let mut push = |segment: Segment| match out.last_mut() {
        Some(last) if same_state(last, &segment) && last.end_ms == segment.start_ms => {
            last.end_ms = segment.end_ms;
        }
        _ => out.push(segment),
    };
    if let [only] = points[..] {
        push(classify((only, only), sessions, gaps, floor_ms));
    } else {
        for (i, point) in points.iter().copied().enumerate() {
            if i > 0 {
                push(classify((points[i - 1], point), sessions, gaps, floor_ms));
            }
            // A zero-length gap is a point of its own, wherever it falls.
            if gaps.iter().any(|g| g.start_ms == point && g.end_ms == point) {
                push(classify((point, point), sessions, gaps, floor_ms));
            }
        }
    }
    out
}

/// Whether any event of the page lies within the gap's bounds. A marker over
/// surviving events must say so; it is never "nothing here".
pub fn gap_overlaps_events(gap: &Gap, event_ts_ms: &[i64]) -> bool {
    event_ts_ms
        .iter()
        .any(|t| gap.start_ms <= *t && *t <= gap.end_ms)
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;
