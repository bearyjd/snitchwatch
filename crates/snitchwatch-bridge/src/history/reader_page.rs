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
    Standing,
};
use super::reader_size::{event_bytes, gap_bytes, segment_bytes, session_bytes, HEADER_BYTES};
use super::session::Session;
use super::timeline::{coverage, gap_overlaps_events, Segment, Window};

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
}

struct Candidate {
    lo_ms: i64,
    gaps: Vec<Gap>,
    sessions: Vec<Session>,
    gap_views: Vec<GapView>,
    session_views: Vec<SessionView>,
    segments: Vec<Segment>,
    bytes: usize,
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

    fn candidate(&self, n: usize, ctx: &Context<'_>) -> Candidate {
        let (lo, hi) = (self.lo_for(n), self.hi_ms);
        let gaps: Vec<Gap> = self
            .gaps
            .iter()
            .filter(|g| overlaps(g.start_ms, g.end_ms, lo, hi))
            .copied()
            .collect();
        let live = ctx.live_session_start_ms;
        let sessions: Vec<Session> = effective(&self.sessions, live, self.watermark_ms)
            .into_iter()
            .filter(|s| overlaps(s.start_ms, s.end_ms.unwrap_or(i64::MAX), lo, hi))
            .collect();
        let segments = coverage(window(lo, hi), &sessions, &gaps, self.floor_ms);
        let event_ts: Vec<i64> = self.events[..n].iter().map(|e| e.ts_ms).collect();
        let mut c = Candidate {
            lo_ms: lo,
            gap_views: Vec::new(),
            session_views: session_views(&sessions, live),
            over_rows: gaps.len() > ctx.limits.gap_rows_per_page
                || sessions.len() > ctx.limits.session_rows_per_page,
            bytes: 0,
            gaps,
            sessions,
            segments,
        };
        c.gap_views = gap_views(&c.gaps, lo, hi, &event_ts);
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
fn largest_fit(loaded: &Loaded, ctx: &Context<'_>) -> Option<usize> {
    let n = loaded.events.len();
    if fits(&loaded.candidate(n, ctx), &ctx.limits) {
        return Some(n);
    }
    if !fits(&loaded.candidate(0, ctx), &ctx.limits) {
        return None;
    }
    // Fewer events, a smaller window, fewer rows and bytes: monotone.
    let (mut ok, mut bad) = (0, n);
    while bad - ok > 1 {
        let mid = ok + (bad - ok) / 2;
        if fits(&loaded.candidate(mid, ctx), &ctx.limits) {
            ok = mid;
        } else {
            bad = mid;
        }
    }
    Some(ok)
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
) -> Page {
    let full = loaded.events.len();
    let (n, forced) = match largest_fit(&loaded, ctx) {
        // Progress: a page that kept no event would give the client the same
        // cursor again. Keep the newest one and trim the ledger instead.
        Some(0) | None if full > 0 => (1, Some(Partial::Gaps)),
        Some(n) => (n, None),
        // Even an empty window carries more ledger rows than a page may:
        // keep the newest, and say so. Never silent.
        None => (0, Some(Partial::Gaps)),
    };
    let mut c = loaded.candidate(n, ctx);
    if forced.is_some() {
        truncate_ledger(&mut c, &loaded, ctx, &loaded.events[..n]);
    }
    let shrunk = n < full;
    let over_rows_at_full = shrunk && loaded.candidate(full, ctx).over_rows;
    loaded.events.truncate(n);
    let partial = forced
        .or(over_rows_at_full.then_some(Partial::Gaps))
        .or(shrunk.then_some(Partial::Bytes))
        .or(loaded.partial);
    let more = loaded.more || shrunk;
    let end = end_state(more, &c.gaps);
    Page {
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
    }
}

fn end_state(more: bool, gaps: &[Gap]) -> End {
    let evicted = gaps.iter().any(|g| g.cause == GapCause::Evicted);
    match (more, evicted) {
        (true, _) => End::More,
        (false, true) => End::RetentionFloor,
        (false, false) => End::OldestRecorded,
    }
}

/// The pathological case: keep the newest rows a page may carry and tile
/// what they cover (anything they leave unexplained shows as such).
fn truncate_ledger(c: &mut Candidate, loaded: &Loaded, ctx: &Context<'_>, kept: &[EventView]) {
    c.gaps.truncate(ctx.limits.gap_rows_per_page);
    c.sessions.truncate(ctx.limits.session_rows_per_page);
    c.segments = coverage(
        window(c.lo_ms, loaded.hi_ms),
        &c.sessions,
        &c.gaps,
        loaded.floor_ms,
    );
    let kept_ts: Vec<i64> = kept.iter().map(|e| e.ts_ms).collect();
    c.gap_views = gap_views(&c.gaps, c.lo_ms, loaded.hi_ms, &kept_ts);
    c.session_views = session_views(&c.sessions, ctx.live_session_start_ms);
    c.bytes = c.bytes_with(kept);
}
