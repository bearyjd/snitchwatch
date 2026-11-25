//! How many bytes a page's rows take, as an **upper bound** of their JSON
//! (PR 11 re-verifies it against the real serializer): every field is sized
//! with its key, quotes, separators and the longest JSON escape of each
//! character (a control character is `\u00XX`, six bytes; a quote or
//! backslash, two). Summing stored lengths would undercount.

use std::time::Duration;

use super::reader::{EventView, GapView, Limits, SessionView};
use super::timeline::Segment;

/// The page header: generation, window, standing labels, summary, cursor.
pub(super) const HEADER_BYTES: usize = 4096;
/// The smallest page limit a reader accepts: the header and room for rows.
pub(super) const MIN_PAGE_BYTES: usize = 2 * HEADER_BYTES;
const INT: usize = 20;
const BOOL: usize = 5;
const NULL: usize = 4;

fn text(s: &str) -> usize {
    2 + s
        .chars()
        .map(|c| match c {
            '"' | '\\' => 2,
            c if c.is_control() => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}

fn opt_text(s: &Option<String>) -> usize {
    s.as_deref().map_or(NULL, text)
}

fn opt_int<T>(v: &Option<T>) -> usize {
    if v.is_some() {
        INT
    } else {
        NULL
    }
}

/// `"key":value,`
fn field(key: &str, value: usize) -> usize {
    key.len() + 2 + 1 + value + 1
}

pub(super) fn event_bytes(e: &EventView) -> usize {
    2 + field("id", INT)
        + field("ts_ms", INT)
        + field("decided_ts_ms", opt_int(&e.decided_ts_ms))
        + field("process_path", opt_text(&e.process_path))
        + field("dst_host", opt_text(&e.dst_host))
        + field("dst_ip", opt_text(&e.dst_ip))
        + field("dst_port", opt_int(&e.dst_port))
        + field("protocol", opt_text(&e.protocol))
        + field("action", text(&e.action))
        + field("answered_by", text(&e.answered_by))
        + field("rule_name", opt_text(&e.rule_name))
        + field("rule_hash", opt_text(&e.rule_hash))
        + field("rule_class", opt_text(&e.rule_class))
        + field("repeat_count", INT)
        + field("src_port", opt_int(&e.src_port))
        + field("src_ip", opt_text(&e.src_ip))
        + field("prompt_outcome", opt_text(&e.prompt_outcome))
        + field("possible_duplicate", BOOL)
}

pub(super) fn gap_bytes(g: &GapView) -> usize {
    2 + field("start_ms", INT)
        + field("end_ms", INT)
        + field("start_exact", BOOL)
        + field("cause", text(g.cause))
        + field("count_min", INT)
        + field("count_is_lower_bound", BOOL)
        + field("part_of_pair", BOOL)
        + field("clipped", BOOL)
        + field("overlaps_events", BOOL)
}

pub(super) fn session_bytes(s: &SessionView) -> usize {
    2 + field("start_ms", INT)
        + field("end_ms", INT.max(NULL))
        + field("end_kind", s.end_kind.map_or(NULL, text))
        + field("running", BOOL)
}

pub(super) fn segment_bytes(s: &Segment) -> usize {
    2 + field("start_ms", INT)
        + field("end_ms", INT)
        + field("kind", 14)
        + field("session", BOOL)
        + field("gaps", 2 + s.gaps.len() * (INT + 1))
}

/// The most a caller may make SQLite wait for a writer's commit, per
/// statement: well under the writer's own 250 ms, and under rusqlite's
/// millisecond conversion, which panics past `i32::MAX`.
const BUSY_WAIT_CEILING: Duration = Duration::from_millis(100);

impl Limits {
    /// These limits, each lowered to the production value if it is higher:
    /// the lock, time and memory ceilings are the reader's own, whatever a
    /// caller passes. A caller can ask for less, never for more.
    pub(super) fn clamped(&self) -> Self {
        let ceiling = Self::default();
        Self {
            events_per_page: self.events_per_page.min(ceiling.events_per_page),
            gap_rows_per_page: self.gap_rows_per_page.min(ceiling.gap_rows_per_page),
            session_rows_per_page: self
                .session_rows_per_page
                .min(ceiling.session_rows_per_page),
            page_bytes: self.page_bytes.min(ceiling.page_bytes),
            statement_budget: self.statement_budget.min(ceiling.statement_budget),
            query_deadline: self.query_deadline.min(ceiling.query_deadline),
            scan_window: self.scan_window.min(ceiling.scan_window),
            scan_rows_max: self.scan_rows_max.min(ceiling.scan_rows_max),
            busy_wait: self.busy_wait.min(BUSY_WAIT_CEILING),
        }
    }
}
