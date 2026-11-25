//! Test fixture for the reader suites: a real store in a tempdir, written
//! through the real writer's connection, read through the real reader.

use super::caps::ProcessPath;
use super::ledger::{record_gap, Gap, GapCause, LedgerLimits};
use super::lifecycle::Generation;
use super::reader::{read_page, Context, Cursor, End, Limits, Outcome, Page, Request};
use super::reader_registry::CancelToken;
use super::record::{insert_event, Action, AnsweredBy, EventRecord};
use super::retention_support::{rig, Rig};
use super::session::{start_session, SessionEnd};

pub(super) const NOW_MS: i64 = 1_000_000_000;

pub(super) struct Fx {
    pub rig: Rig,
    pub db: std::path::PathBuf,
    pub generation: Generation,
    pub cancel: CancelToken,
    pub limits: Limits,
    pub live_session_start_ms: Option<i64>,
    pub standing: Option<super::gaps::StandingLabels>,
    pub now_ms: i64,
}

impl Fx {
    pub fn new() -> Self {
        let rig = rig();
        Self {
            db: rig.location.db_path(),
            rig,
            generation: Generation::default(),
            cancel: CancelToken::default(),
            limits: Limits::default(),
            live_session_start_ms: None,
            standing: None,
            now_ms: NOW_MS,
        }
    }

    pub fn ctx(&self) -> Context<'_> {
        Context {
            db_path: &self.db,
            generation: &self.generation,
            cancel: &self.cancel,
            now_ms: self.now_ms,
            live_session_start_ms: self.live_session_start_ms,
            standing: self.standing,
            limits: self.limits,
        }
    }

    pub fn conn(&self) -> &rusqlite::Connection {
        self.rig.engine.connection()
    }

    pub fn events(&self, timestamps: impl IntoIterator<Item = i64>) {
        for ts in timestamps {
            let mut e = EventRecord::new(ts, Action::Allow, AnsweredBy::Rule);
            e.process_path = Some(ProcessPath::new("/usr/bin/curl"));
            insert_event(self.conn(), &e).unwrap();
        }
    }

    pub fn gap(&self, start: i64, end: i64, cause: GapCause, count: u64) {
        let gap = Gap {
            start_ms: start,
            end_ms: end,
            start_exact: true,
            cause,
            count,
        };
        let limits = LedgerLimits::from_budget(self.rig.engine.budget());
        record_gap(self.conn(), &gap, &limits).unwrap();
    }

    pub fn session(&self, start: i64) {
        let limits = LedgerLimits::from_budget(self.rig.engine.budget());
        start_session(self.conn(), &limits, start, start).unwrap();
    }

    pub fn end_session(&self, end: i64, kind: SessionEnd) {
        super::session::end_session(self.conn(), end, kind).unwrap();
    }

    pub fn read(&self, cursor: Option<Cursor>, limit: usize) -> Outcome {
        read_page(&Request { cursor, limit }, &self.ctx())
    }

    pub fn page(&self, cursor: Option<Cursor>, limit: usize) -> Page {
        match self.read(cursor, limit) {
            Outcome::Page(page) => *page,
            other => panic!("expected a page, got {other:?}"),
        }
    }

    /// Every page from the head to the end, newest first.
    pub fn all_pages(&self, limit: usize) -> Vec<Page> {
        let mut pages = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.page(cursor, limit);
            if page.end == End::More {
                assert!(
                    page.next_cursor.is_some() && page.next_cursor != cursor,
                    "End::More without progress: cursor {cursor:?} -> {:?}",
                    page.next_cursor
                );
            }
            cursor = page.next_cursor;
            let done = page.next_cursor.is_none();
            pages.push(page);
            assert!(pages.len() < 10_000, "paging never ends");
            if done {
                return pages;
            }
        }
    }
}
