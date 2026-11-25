//! Building a page from what was read: the window, the timeline over it, and
//! the shrink that keeps the page inside its row and byte bounds.
//!
//! The ledger is read for the widest window the loaded events allow; the
//! page then keeps the **largest number of events** whose window still fits
//! (gap rows, session rows, bytes). Events are dropped from the old end, so
//! the cursor stays at the last kept one and no gap row is ever dropped or
//! truncated: a smaller window simply covers fewer of them.

use super::gap_counts::{pair_flags, summarize};
use super::ledger::{Gap, GapCause};
use super::reader::{
    Context, Cursor, End, EventView, GapView, Label, Limits, Page, Partial, Request, SessionView,
    Standing, Unavail, UnavailKind,
};
use super::reader_size::{event_bytes, gap_bytes, segment_bytes, session_bytes, HEADER_BYTES};
use super::reader_sql::Control;
use super::session::Session;
use super::timeline::{coalesce, coverage_masked, gap_overlaps_events, Segment, Window};

/// What the two read stages produced.
pub(super) struct Loaded {
    pub events: Vec<EventView>,
    /// Older events exist (or may: a partial page).
    pub more: bool,
    pub partial: Option<Partial>,
    pub floor_ms: Option<i64>,
    /// Oldest start of any ledger row (for the end of history).
    pub ledger_start_ms: Option<i64>,
    pub gaps: Vec<Gap>,
    pub sessions: Vec<Session>,
    pub watermark_ms: Option<i64>,
    pub hi_ms: i64,
    /// The query ran out of time before the ledger was read: its rows are
    /// unknown, and the window is said to be unexplained.
    pub ledger_unknown: bool,
}

/// Most spans of dropped rows a page's coverage is cut at (see
/// [`coalesce`]): what bounds the segments a trimmed page carries.
const OMITTED_SPANS_MAX: usize = 16;

/// The ledger rows that overlap one candidate window, before any is carried.
#[derive(Clone)]
struct Rows {
    lo_ms: i64,
    gaps: Vec<Gap>,
    sessions: Vec<Session>,
}

struct Candidate {
    lo_ms: i64,
    gaps: Vec<Gap>,
    gap_views: Vec<GapView>,
    session_views: Vec<SessionView>,
    segments: Vec<Segment>,
    bytes: usize,
    /// More rows overlap the window than a page may carry. Then nothing
    /// else is built: the window is too big to describe.
    over_rows: bool,
}

fn overlaps(start: i64, end: i64, lo: i64, hi: i64) -> bool {
    end >= lo && start <= hi
}

/// An open session that is not the live one ran only to the watermark: after
/// that nothing recorded, so it must not claim the stretch to the head.
fn effective(sessions: &[Session], live: Option<i64>, watermark: Option<i64>) -> Vec<Session> {
    sessions
        .iter()
        .map(|s| match (s.end_ms, live) {
            (None, Some(start)) if start == s.start_ms => *s,
            (None, _) => Session {
                end_ms: Some(watermark.map_or(s.start_ms, |w| w.max(s.start_ms))),
                ..*s
            },
            _ => *s,
        })
        .collect()
}

fn window(lo: i64, hi: i64) -> Window {
    Window::new(lo, hi).unwrap_or(Window {
        lo_ms: hi,
        hi_ms: hi,
    })
}

impl Loaded {
    pub(super) fn empty(hi_ms: i64) -> Self {
        Self {
            events: Vec::new(),
            more: false,
            partial: None,
            floor_ms: None,
            ledger_start_ms: None,
            gaps: Vec::new(),
            sessions: Vec::new(),
            watermark_ms: None,
            hi_ms,
            ledger_unknown: false,
        }
    }

    /// The widest window the loaded events allow: all the ledger worth
    /// reading for this page.
    pub(super) fn widest_lo(&self) -> i64 {
        self.lo_for(self.events.len())
    }

    /// Window low edge when `n` events are kept.
    fn lo_for(&self, n: usize) -> i64 {
        let end_of_history = n == self.events.len() && !self.more;
        if end_of_history {
            let oldest = [self.floor_ms, self.ledger_start_ms]
                .into_iter()
                .flatten()
                .min();
            return oldest.map_or(self.hi_ms, |o| o.min(self.hi_ms));
        }
        match n {
            0 => self.hi_ms,
            n => self.events[n - 1].ts_ms.min(self.hi_ms),
        }
    }

    /// The rows overlapping the window of `n` kept events.
    fn rows(&self, n: usize, ctx: &Context<'_>) -> Rows {
        let (lo, hi) = (self.lo_for(n), self.hi_ms);
        if self.ledger_unknown {
            return Rows {
                lo_ms: lo,
                gaps: Vec::new(),
                sessions: Vec::new(),
            };
        }
        let gaps = self
            .gaps
            .iter()
            .filter(|g| overlaps(g.start_ms, g.end_ms, lo, hi))
            .copied()
            .collect();
        let sessions = effective(&self.sessions, ctx.live_session_start_ms, self.watermark_ms)
            .into_iter()
            .filter(|s| overlaps(s.start_ms, s.end_ms.unwrap_or(i64::MAX), lo, hi))
            .collect();
        Rows {
            lo_ms: lo,
            gaps,
            sessions,
        }
    }

    /// The page for `n` events as it would be carried whole. The row caps are
    /// checked before any segment or gap view is built: a ledger that cannot
    /// fit costs a count, not a tiling.
    fn candidate(&self, n: usize, ctx: &Context<'_>) -> Candidate {
        let rows = self.rows(n, ctx);
        let bounds = ctx.bounds();
        let over_rows = rows.gaps.len() > bounds.gap_rows_per_page
            || rows.sessions.len() > bounds.session_rows_per_page;
        if over_rows {
            return Candidate {
                lo_ms: rows.lo_ms,
                gaps: rows.gaps,
                gap_views: Vec::new(),
                session_views: Vec::new(),
                segments: Vec::new(),
                bytes: usize::MAX,
                over_rows,
            };
        }
        let (kg, ks) = (rows.gaps.len(), rows.sessions.len());
        self.carrying(n, rows, (kg, ks), ctx)
    }

    /// The page for `n` events carrying the newest `keep` (gaps, sessions)
    /// of `rows`; what the rest covered is reported as unexplained.
    fn carrying(&self, n: usize, rows: Rows, keep: (usize, usize), ctx: &Context<'_>) -> Candidate {
        let Rows {
            lo_ms: lo,
            mut gaps,
            mut sessions,
        } = rows;
        let hi = self.hi_ms;
        let left_gaps = gaps.split_off(keep.0.min(gaps.len()));
        let left_sessions = sessions.split_off(keep.1.min(sessions.len()));
        let dropped = coalesce(
            left_gaps.iter().map(|g| (g.start_ms, g.end_ms)).chain(
                left_sessions
                    .iter()
                    .map(|s| (s.start_ms, s.end_ms.unwrap_or(hi))),
            ),
            OMITTED_SPANS_MAX,
        );
        let segments = coverage_masked(window(lo, hi), &sessions, &gaps, self.floor_ms, &dropped);
        let event_ts: Vec<i64> = self.events[..n].iter().map(|e| e.ts_ms).collect();
        let mut c = Candidate {
            lo_ms: lo,
            gap_views: gap_views(&gaps, lo, hi, &event_ts),
            session_views: session_views(&sessions, ctx.live_session_start_ms),
            over_rows: false,
            bytes: 0,
            gaps,
            segments,
        };
        c.bytes = c.bytes_with(&self.events[..n]);
        c
    }
}

impl Candidate {
    fn bytes_with(&self, events: &[EventView]) -> usize {
        HEADER_BYTES
            + events.iter().map(event_bytes).sum::<usize>()
            + self.gap_views.iter().map(gap_bytes).sum::<usize>()
            + self.session_views.iter().map(session_bytes).sum::<usize>()
            + self.segments.iter().map(segment_bytes).sum::<usize>()
    }
}

fn fits(c: &Candidate, limits: &Limits) -> bool {
    !c.over_rows && c.bytes <= limits.page_bytes
}

/// The most events whose window fits; `None` if even zero events do not.
/// Out of time, the best fit found so far stands.
fn largest_fit(loaded: &Loaded, ctx: &Context<'_>, ctl: &Control) -> Option<usize> {
    let n = loaded.events.len();
    if fits(&loaded.candidate(n, ctx), &ctx.bounds()) {
        return Some(n);
    }
    if !fits(&loaded.candidate(0, ctx), &ctx.bounds()) {
        return None;
    }
    // Fewer events, a smaller window, fewer rows and bytes: monotone.
    let (mut ok, mut bad) = (0, n);
    while bad - ok > 1 && !ctl.deadline_passed() {
        let mid = ok + (bad - ok) / 2;
        if fits(&loaded.candidate(mid, ctx), &ctx.bounds()) {
            ok = mid;
        } else {
            bad = mid;
        }
    }
    Some(ok)
}

/// The pathological case: `n` events whose window holds more ledger than a
/// page may carry. Keep the newest rows that fit the row and byte bounds,
/// halving until they do; what the rest covered shows as unexplained. If
/// not even the events and a bare tiling fit, the page cannot be made.
fn trimmed(loaded: &Loaded, n: usize, ctx: &Context<'_>) -> Result<Candidate, Unavail> {
    let rows = loaded.rows(n, ctx);
    let limits = &ctx.bounds();
    let mut keep = (
        rows.gaps.len().min(limits.gap_rows_per_page),
        rows.sessions.len().min(limits.session_rows_per_page),
    );
    loop {
        let c = loaded.carrying(n, rows.clone(), keep, ctx);
        if c.bytes <= limits.page_bytes {
            return Ok(c);
        }
        if keep == (0, 0) {
            return Err(unavailable_oversized());
        }
        keep = (keep.0 / 2, keep.1 / 2);
    }
}

fn unavailable_oversized() -> Unavail {
    Unavail {
        reason: "one history event is larger than the page byte limit".into(),
        retry: false,
        kind: UnavailKind::OversizedEvent,
    }
}

fn label(on: bool, available: bool) -> Label {
    match (available, on) {
        (false, _) => Label::Unavailable,
        (true, true) => Label::Yes,
        (true, false) => Label::No,
    }
}

fn standing(ctx: &Context<'_>) -> Standing {
    let labels = ctx.standing.unwrap_or_default();
    let on = ctx.standing.is_some();
    Standing {
        scope: "current_session",
        session_start_ms: ctx.live_session_start_ms,
        nolog: label(labels.nolog, on),
        stock_default_unobservable: label(labels.stock_default_unobservable, on),
        no_gui_prompts: label(labels.no_gui_prompts, on),
        earlier_sessions: "unknown",
    }
}

fn gap_views(gaps: &[Gap], lo: i64, hi: i64, event_ts: &[i64]) -> Vec<GapView> {
    let paired = pair_flags(gaps);
    gaps.iter()
        .zip(paired)
        .map(|(g, part_of_pair)| GapView {
            start_ms: g.start_ms,
            end_ms: g.end_ms,
            start_exact: g.start_exact,
            cause: g.cause.as_str(),
            count_min: g.count,
            count_is_lower_bound: true,
            part_of_pair,
            clipped: g.start_ms < lo || g.end_ms > hi,
            overlaps_events: gap_overlaps_events(g, event_ts),
        })
        .collect()
}

fn session_views(sessions: &[Session], live: Option<i64>) -> Vec<SessionView> {
    sessions
        .iter()
        .map(|s| SessionView {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            end_kind: s.end.map(super::session::SessionEnd::as_str),
            running: s.end_ms.is_none() && live == Some(s.start_ms),
        })
        .collect()
}

fn cursor_after(events: &[EventView], generation: u64, request: &Request) -> Option<Cursor> {
    match events.last() {
        Some(e) => Some(Cursor {
            generation,
            ts_ms: e.ts_ms,
            id: e.id,
        }),
        None => request.cursor,
    }
}

pub(super) fn assemble(
    mut loaded: Loaded,
    generation: u64,
    request: &Request,
    ctx: &Context<'_>,
    ctl: &Control,
) -> Result<Page, Unavail> {
    let full = loaded.events.len();
    let (n, forced) = match largest_fit(&loaded, ctx, ctl) {
        // Progress: a page that kept no event would hand the client the same
        // cursor again. Keep the newest one and trim the ledger instead.
        Some(0) | None if full > 0 => (1, Some(Partial::Gaps)),
        Some(n) => (n, None),
        // Even an empty window is over the row caps: trim it, and say so.
        None => (0, Some(Partial::Gaps)),
    };
    // Read before any trimming: a dropped marker still says what was there.
    let evicted = loaded
        .rows(n, ctx)
        .gaps
        .iter()
        .any(|g| g.cause == GapCause::Evicted);
    let c = match forced {
        Some(_) => trimmed(&loaded, n, ctx)?,
        None => loaded.candidate(n, ctx),
    };
    let shrunk = n < full;
    let over_rows_at_full = shrunk && loaded.candidate(full, ctx).over_rows;
    loaded.events.truncate(n);
    let partial = forced
        .or(over_rows_at_full.then_some(Partial::Gaps))
        .or(shrunk.then_some(Partial::Bytes))
        .or(loaded.partial);
    let more = loaded.more || shrunk;
    let end = end_state(more, evicted);
    Ok(Page {
        generation,
        at_head: request.cursor.is_none(),
        next_cursor: more
            .then(|| cursor_after(&loaded.events, generation, request))
            .flatten(),
        summary: summarize(&c.gaps).into(),
        gaps: c.gap_views,
        sessions: c.session_views,
        coverage: c.segments,
        standing: standing(ctx),
        retention_floor_ms: loaded.floor_ms,
        window_lo_ms: c.lo_ms,
        window_hi_ms: loaded.hi_ms,
        events: loaded.events,
        partial,
        end,
        bytes: c.bytes,
    })
}

fn end_state(more: bool, evicted: bool) -> End {
    match (more, evicted) {
        (true, _) => End::More,
        (false, true) => End::RetentionFloor,
        (false, false) => End::OldestRecorded,
    }
}
