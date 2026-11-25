//! Connection history PR 9: the reader. Read-only, bounded, honest.
//!
//! [`read_page`] answers one request with one [`Page`]: up to 200 events,
//! newest first, with the gaps, sessions and standing labels that explain
//! every instant of the page's window ([`super::timeline`]). It opens its own
//! read-only connection ([`crate::sqlite_file::open_owner_only_readonly`]),
//! imports no write type, and holds no lock between statements: in rollback-
//! journal mode a reader's lock delays the writer's commit, which gives up
//! after 250 ms, so every statement is short (see [`Limits`]) and runs in
//! autocommit.
//!
//! Unavailable is never empty: a missing file, a hot journal, a refused
//! schema or a control operation each give [`Outcome::Unavailable`], never a
//! page with `events: []`.

use std::time::Duration;

use serde::Serialize;

use super::gap_counts::Summary;
use super::gaps::StandingLabels;
use super::lifecycle::Generation;
use super::reader_open::{from_stop, open_validated};
use super::reader_page::{assemble, Loaded};
use super::reader_registry::CancelToken;
use super::reader_size::{event_bytes, HEADER_BYTES, MIN_PAGE_BYTES};
use super::reader_sql::{
    events_chunk, floor, gaps_overlapping, ledger_start, sessions_overlapping, watermark, Control,
    Stop, DUPLICATE_PROBE_CAP,
};
use super::timeline::Segment;
use super::{load_state_read_only, Location, State};

/// The safety bounds (D8). Constants, not settings (Decision 6): a struct
/// whose `Default` is production, so a test can shrink a budget without a
/// test-only branch in the read path.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Events per page.
    pub events_per_page: usize,
    pub gap_rows_per_page: usize,
    pub session_rows_per_page: usize,
    /// Upper bound of the serialized page.
    pub page_bytes: usize,
    /// One SQL statement: kept under the writer's 250 ms busy timeout.
    pub statement_budget: Duration,
    /// The whole query.
    pub query_deadline: Duration,
    /// Index entries one statement may return.
    pub scan_window: usize,
    /// Index entries examined by the whole query.
    pub scan_rows_max: usize,
    /// How long SQLite itself may wait for a writer's commit, per statement.
    pub busy_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            events_per_page: 200,
            gap_rows_per_page: 100,
            session_rows_per_page: 100,
            page_bytes: 256 * 1024,
            statement_budget: Duration::from_millis(100),
            query_deadline: Duration::from_secs(2),
            scan_window: 512,
            scan_rows_max: 20_000,
            busy_wait: Duration::from_millis(20),
        }
    }
}

/// Where the next page starts: a value, never a row reference, so a cursor
/// whose row was evicted still resumes at the next older surviving key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Cursor {
    pub generation: u64,
    pub ts_ms: i64,
    pub id: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct Request {
    pub cursor: Option<Cursor>,
    /// Clamped to [`Limits::events_per_page`], never an error.
    pub limit: usize,
}

/// What the reader needs from its caller.
pub struct Context<'a> {
    /// Where history lives; the reader asks it the same question
    /// [`super::open_store`] does (is history on?) before it opens the file.
    pub location: &'a Location,
    pub generation: &'a Generation,
    pub cancel: &'a CancelToken,
    pub now_ms: i64,
    /// Start of the live capture's session, if one is running: the only open
    /// session that runs to the head.
    pub live_session_start_ms: Option<i64>,
    /// `None`: no capture is running, the labels are unavailable.
    pub standing: Option<StandingLabels>,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventView {
    pub id: i64,
    pub ts_ms: i64,
    pub decided_ts_ms: Option<i64>,
    pub process_path: Option<String>,
    pub dst_host: Option<String>,
    pub dst_ip: Option<String>,
    pub dst_port: Option<i64>,
    pub protocol: Option<String>,
    pub action: String,
    pub answered_by: String,
    pub rule_name: Option<String>,
    pub rule_hash: Option<String>,
    pub rule_class: Option<String>,
    pub repeat_count: i64,
    pub src_port: Option<i64>,
    pub src_ip: Option<String>,
    pub prompt_outcome: Option<String>,
    pub possible_duplicate: bool,
    /// A stored field was longer than the reader keeps and was cut: the row
    /// is not what was recorded (a malformed file, never a writer's row).
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GapView {
    pub start_ms: i64,
    pub end_ms: i64,
    pub start_exact: bool,
    pub cause: &'static str,
    /// At least this many (items, or restarts for `uncertain`).
    pub count_min: u64,
    /// Always true: no gap count is exact.
    pub count_is_lower_bound: bool,
    /// Half of a bracketing outage pair: "at least 1 on each side".
    pub part_of_pair: bool,
    /// The row extends past this page's window (display only; the row's own
    /// bounds above are never shrunk).
    pub clipped: bool,
    /// Events this page returns lie inside it: "some were removed", never
    /// "nothing here".
    pub overlaps_events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionView {
    pub start_ms: i64,
    /// Where recording is known to have run to. An open session that is not
    /// the live one ends at the durable watermark.
    pub end_ms: Option<i64>,
    pub end_kind: Option<&'static str>,
    pub running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Label {
    Yes,
    No,
    Unavailable,
}

/// What history cannot observe, as standing notes. In memory only, so they
/// describe the current session; earlier sessions are `unknown`, not `false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Standing {
    pub scope: &'static str,
    pub session_start_ms: Option<i64>,
    pub nolog: Label,
    pub stock_default_unobservable: Label,
    pub no_gui_prompts: Label,
    pub earlier_sessions: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Partial {
    Bytes,
    Time,
    Scan,
    Busy,
    /// More gap rows overlap the window than a page may carry.
    Gaps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum End {
    /// An older page exists: use `next_cursor`.
    More,
    /// The oldest surviving event was reached and retention has removed older
    /// ones (an `evicted` row exists).
    RetentionFloor,
    /// The oldest recorded event; nothing older was ever recorded.
    OldestRecorded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Page {
    pub generation: u64,
    pub at_head: bool,
    pub events: Vec<EventView>,
    pub coverage: Vec<Segment>,
    pub gaps: Vec<GapView>,
    pub sessions: Vec<SessionView>,
    pub standing: Standing,
    pub retention_floor_ms: Option<i64>,
    pub window_lo_ms: i64,
    pub window_hi_ms: i64,
    pub partial: Option<Partial>,
    pub end: End,
    pub next_cursor: Option<Cursor>,
    pub summary: SummaryView,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SummaryView {
    /// A lower bound for this page's window only: rows the window clips
    /// appear on every page they overlap, so do not add pages together.
    pub lost_at_least: u64,
    pub gap_rows: usize,
    pub may_overcount_by_pairs: usize,
}

impl From<Summary> for SummaryView {
    fn from(s: Summary) -> Self {
        Self {
            lost_at_least: s.lost_at_least,
            gap_rows: s.gap_rows,
            may_overcount_by_pairs: s.may_overcount_by_pairs,
        }
    }
}

/// History cannot be read right now. Never rendered as "empty".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavail {
    pub reason: String,
    /// Worth retrying soon (a writer's commit, a journal to recover).
    pub retry: bool,
    /// Which refusal, for a caller that must tell them apart without
    /// reading the reason.
    pub kind: UnavailKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailKind {
    /// Missing, busy, damaged or refused file.
    Other,
    /// History is off: nothing was opened.
    Off,
    /// The request costs more than one query may spend, even at one event:
    /// retrying the same request cannot help.
    TooExpensive,
    /// One event is larger than the page byte limit.
    OversizedEvent,
    /// The limits cannot hold even an empty page.
    LimitsTooSmall,
}

#[derive(Debug)]
pub enum Outcome {
    Page(Box<Page>),
    Unavailable(Unavail),
    /// The cursor is from before a clear, disable or retention change.
    StaleGeneration,
    /// A control operation ran while the page was built: nothing is
    /// delivered (G1).
    Superseded,
    Cancelled,
}

/// One read, in stages: [`Self::open`], [`Self::load_events`],
/// [`Self::load_ledger`], [`Self::finish`]. [`read_page`] runs them in order;
/// the stages are separate so a test can move the world (a clear, a cancel, a
/// commit) between two of them without a hook in the read path.
pub struct Read<'a> {
    request: Request,
    ctx: &'a Context<'a>,
    generation: u64,
    conn: rusqlite::Connection,
    control: Control,
    loaded: Loaded,
}

/// What one `load_events` run has collected so far.
#[derive(Default)]
struct Collect {
    limit: usize,
    rows: Vec<EventView>,
    after: Option<(i64, i64)>,
    statements: usize,
    scanned: usize,
    bytes: usize,
}

impl Context<'_> {
    /// The limits this read runs under: the caller's, clamped to the
    /// reader's hard ceilings.
    pub(super) fn bounds(&self) -> Limits {
        self.limits.clamped()
    }
}

fn unavailable(u: Unavail) -> Outcome {
    Outcome::Unavailable(u)
}

/// Neither a control operation nor a cancel has landed since `generation`.
fn still_current(ctx: &Context<'_>, generation: u64, control: &Control) -> Result<(), Outcome> {
    if ctx.generation.current() != generation {
        return Err(Outcome::Superseded);
    }
    if control.cancelled() {
        return Err(Outcome::Cancelled);
    }
    Ok(())
}

/// One ledger statement, unless the query's time is already spent: a short
/// statement could finish below the progress handler's first tick.
fn before_statement(control: &Control) -> Result<(), Stop> {
    if control.deadline_passed() {
        Err(Stop::Deadline)
    } else {
        Ok(())
    }
}

fn refusal(kind: UnavailKind, reason: String) -> Outcome {
    Outcome::Unavailable(Unavail {
        reason,
        retry: false,
        kind,
    })
}

/// A cut by time or by a writer's commit that read no event: the same
/// request can succeed a moment later.
fn try_again(why: Partial) -> Outcome {
    let cause = match why {
        Partial::Busy => "a writer is committing",
        _ => "the read ran out of time",
    };
    Outcome::Unavailable(Unavail {
        reason: format!("history is busy ({cause}): ask again"),
        retry: true,
        kind: UnavailKind::Other,
    })
}

/// A scan-cap cut that read no event at all: the same retry would repeat the
/// same expensive scan, so say so instead.
fn too_expensive() -> Outcome {
    refusal(
        UnavailKind::TooExpensive,
        "history read is too expensive for one request: ask for a smaller page".into(),
    )
}

pub(super) fn oversized() -> Outcome {
    refusal(
        UnavailKind::OversizedEvent,
        "one history event is larger than the page byte limit".into(),
    )
}

fn stopped(stop: Stop) -> Outcome {
    match stop {
        Stop::Cancelled => Outcome::Cancelled,
        other => unavailable(from_stop(other)),
    }
}

impl<'a> Read<'a> {
    /// Refuses a cursor from another generation, a cancelled query, and
    /// anything that is not the history database; then holds a read-only
    /// connection and no lock.
    pub fn open(request: &Request, ctx: &'a Context<'a>) -> Result<Self, Outcome> {
        let generation = ctx.generation.current();
        if request.cursor.is_some_and(|c| c.generation != generation) {
            return Err(Outcome::StaleGeneration);
        }
        if ctx.cancel.is_cancelled() {
            return Err(Outcome::Cancelled);
        }
        if ctx.bounds().page_bytes < MIN_PAGE_BYTES {
            return Err(refusal(
                UnavailKind::LimitsTooSmall,
                format!("the page byte limit is under {MIN_PAGE_BYTES}, less than a page"),
            ));
        }
        // The answer `open_store` gives, before any file is touched.
        match load_state_read_only(ctx.location) {
            State::Enabled(_) => {}
            State::Off => return Err(refusal(UnavailKind::Off, "history is off".into())),
            State::Unavailable(why) => return Err(refusal(UnavailKind::Other, why.0)),
        }
        let control = Control::new(ctx.cancel, &ctx.bounds());
        // A cancel that lands while the file is being checked is a cancel, not
        // an unavailable store (a cancel is sticky).
        let conn = open_validated(&ctx.location.db_path(), &control, ctx.bounds().busy_wait)
            .map_err(|u| {
                if ctx.cancel.is_cancelled() {
                    Outcome::Cancelled
                } else {
                    unavailable(u)
                }
            })?;
        ctx.cancel.attach(conn.get_interrupt_handle());
        let hi_ms = request.cursor.map_or(ctx.now_ms, |c| c.ts_ms);
        Ok(Self {
            request: *request,
            ctx,
            generation,
            conn,
            control,
            loaded: Loaded::empty(hi_ms),
        })
    }

    fn check(&self) -> Result<(), Outcome> {
        still_current(self.ctx, self.generation, &self.control)
    }

    /// Up to a page of events, newest first, as short statements.
    pub fn load_events(&mut self) -> Result<(), Outcome> {
        self.check()?;
        let limit = self
            .request
            .limit
            .clamp(1, self.ctx.bounds().events_per_page.max(1));
        let mut run = Collect {
            limit,
            after: self.request.cursor.map(|c| (c.ts_ms, c.id)),
            ..Collect::default()
        };
        while self.step(&mut run)? {}
        if run.rows.len() > limit {
            run.rows.truncate(limit);
            self.loaded.more = true;
        }
        self.loaded.events = run.rows;
        Ok(())
    }

    /// Ends the page early, with an older page to follow. A cut that read no
    /// event is never a page (it would hand the client the same cursor
    /// again): a scan-cap cut says the request is too big; a writer's commit
    /// or a time cut says to ask again.
    fn cut(&mut self, why: Partial, run: &Collect) -> Result<bool, Outcome> {
        if run.rows.is_empty() {
            return Err(match why {
                Partial::Scan => too_expensive(),
                _ => try_again(why),
            });
        }
        self.loaded.partial = Some(why);
        self.loaded.more = true;
        Ok(false)
    }

    /// One statement's worth. `Ok(true)`: read on.
    fn step(&mut self, run: &mut Collect) -> Result<bool, Outcome> {
        let limits = self.ctx.bounds();
        if run.statements > 0 && self.control.deadline_passed() {
            return self.cut(Partial::Time, run);
        }
        if run.scanned >= limits.scan_rows_max {
            return self.cut(Partial::Scan, run);
        }
        self.check()?;
        let want = run
            .limit
            .saturating_add(1)
            .saturating_sub(run.rows.len())
            .min(limits.scan_window.max(1))
            .min(limits.scan_rows_max - run.scanned);
        let chunk = events_chunk(
            &self.conn,
            &self.control,
            run.after,
            want,
            DUPLICATE_PROBE_CAP,
        );
        run.statements += 1;
        run.scanned += chunk.rows.len();
        let exhausted = chunk.rows.len() < want && chunk.stop.is_none();
        if !self.take_rows(run, chunk.rows)? {
            return self.cut(Partial::Bytes, run);
        }
        match chunk.stop {
            Some(stop) => self.on_stop(stop, run),
            None => Ok(!exhausted && run.rows.len() <= run.limit),
        }
    }

    /// Keeps the rows that fit the byte bound. `Ok(false)`: one did not. An
    /// event that alone does not fit is refused: no page may exceed the bound.
    fn take_rows(&self, run: &mut Collect, rows: Vec<EventView>) -> Result<bool, Outcome> {
        let room = self.ctx.bounds().page_bytes.saturating_sub(HEADER_BYTES);
        for row in rows {
            let size = event_bytes(&row);
            if run.rows.is_empty() && size > room {
                return Err(oversized());
            }
            run.bytes = run.bytes.saturating_add(size);
            if run.bytes > room {
                return Ok(false);
            }
            run.after = Some((row.ts_ms, row.id));
            run.rows.push(row);
        }
        Ok(true)
    }

    fn on_stop(&mut self, stop: Stop, run: &Collect) -> Result<bool, Outcome> {
        match stop {
            Stop::Cancelled => Err(Outcome::Cancelled),
            Stop::Statement | Stop::Deadline => self.cut(Partial::Time, run),
            Stop::Busy => self.cut(Partial::Busy, run),
            fail @ Stop::Fail { .. } => Err(stopped(fail)),
        }
    }

    /// The gap and session rows that explain the page's window, read after
    /// the events so an eviction in between shows as a marker, not a hole.
    ///
    /// If the query's time runs out here, the events already collected are
    /// kept with their cursor and the ledger is marked unknown (the page's
    /// window is then `Unexplained`, never `Recorded`); with no events there
    /// is nothing to keep and the read is unavailable.
    pub fn load_ledger(&mut self) -> Result<(), Outcome> {
        self.check()?;
        let loaded = &mut self.loaded;
        if let Some(newest) = loaded.events.first() {
            loaded.hi_ms = self
                .request
                .cursor
                .map_or(self.ctx.now_ms.max(newest.ts_ms), |c| c.ts_ms);
        } else if self.request.cursor.is_none() {
            loaded.hi_ms = self.ctx.now_ms;
        }
        match read_ledger(&self.conn, &self.control, loaded) {
            Ok(()) => {}
            Err(Stop::Statement | Stop::Deadline) if !loaded.events.is_empty() => {
                loaded.ledger_unknown = true;
                loaded.partial = Some(Partial::Time);
                loaded.more = true;
            }
            Err(stop) => return Err(stopped(stop)),
        }
        self.check()
    }

    /// The page, unless a control operation ran meanwhile (G1: nothing is
    /// delivered from an older generation).
    pub fn finish(self) -> Outcome {
        match self.build() {
            Ok(built) => built.deliver(),
            Err(outcome) => outcome,
        }
    }

    /// Assembles the page: the timeline, the summary, the fit to the bounds.
    /// A separate stage from [`Built::deliver`] so a test can move the world
    /// between the two.
    pub fn build(self) -> Result<Built<'a>, Outcome> {
        self.check()?;
        let Self {
            request,
            ctx,
            generation,
            control,
            loaded,
            ..
        } = self;
        let page = assemble(loaded, generation, &request, ctx, &control).map_err(unavailable)?;
        Ok(Built {
            page: Box::new(page),
            ctx,
            generation,
            control,
        })
    }
}

/// An assembled page that has not been delivered yet.
pub struct Built<'a> {
    page: Box<Page>,
    ctx: &'a Context<'a>,
    generation: u64,
    control: Control,
}

impl Built<'_> {
    /// Delivers the page unless a control operation or a cancel landed while
    /// it was assembled (G1): assembly is not instant.
    pub fn deliver(self) -> Outcome {
        match still_current(self.ctx, self.generation, &self.control) {
            Ok(()) => Outcome::Page(self.page),
            Err(outcome) => outcome,
        }
    }
}

fn read_ledger(
    conn: &rusqlite::Connection,
    ctl: &Control,
    loaded: &mut Loaded,
) -> Result<(), Stop> {
    before_statement(ctl)?;
    loaded.floor_ms = floor(conn, ctl)?;
    before_statement(ctl)?;
    loaded.ledger_start_ms = ledger_start(conn, ctl)?;
    let lo = loaded.widest_lo();
    before_statement(ctl)?;
    loaded.gaps = gaps_overlapping(conn, ctl, lo, loaded.hi_ms)?;
    before_statement(ctl)?;
    loaded.sessions = sessions_overlapping(conn, ctl, lo, loaded.hi_ms)?;
    before_statement(ctl)?;
    loaded.watermark_ms = watermark(conn, ctl)?;
    Ok(())
}

pub fn read_page(request: &Request, ctx: &Context<'_>) -> Outcome {
    let mut read = match Read::open(request, ctx) {
        Ok(read) => read,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = read.load_events().and_then(|()| read.load_ledger()) {
        return outcome;
    }
    read.finish()
}
