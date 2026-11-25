//! How many bytes a page's rows take, as an **upper bound** of their JSON
//! (PR 11 re-verifies it against the real serializer): every field is sized
//! with its key, quotes, separators and the longest JSON escape of each
//! character (a control character is `\u00XX`, six bytes; a quote or
//! backslash, two). Summing stored lengths would undercount.

use super::reader::{EventView, GapView, SessionView};
use super::timeline::Segment;

/// The page header: generation, window, standing labels, summary, cursor.
pub(super) const HEADER_BYTES: usize = 4096;
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
