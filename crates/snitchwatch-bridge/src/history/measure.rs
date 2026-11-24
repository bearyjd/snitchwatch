//! The byte measure admission and retention share (owner decision 2026-11-24,
//! option 2c): what a row counts for against its process's share of the
//! store. One formula in Rust ([`estimate_bytes`], charged at admission) and
//! the same one in SQL ([`row_bytes_sql`], summed by retention), so the two
//! cannot disagree about what "25% of retained bytes" means.

use super::record::EventRecord;

/// Bytes a record counts for before its strings (the row's fixed columns and
/// its index entries).
pub const ROW_OVERHEAD: u64 = 160;

/// The text columns that count by length, in the order [`estimate_bytes`]
/// adds them.
const TEXT_COLUMNS: [&str; 5] = [
    "process_path",
    "dst_host",
    "rule_name",
    "protocol",
    "rule_class",
];

/// What `r` counts for against its process's share of retained bytes.
pub fn estimate_bytes(r: &EventRecord) -> u64 {
    let len = |n: Option<usize>| n.unwrap_or(0) as u64;
    ROW_OVERHEAD
        + len(r.process_path.as_ref().map(|v| v.as_str().len()))
        + len(r.dst_host.as_ref().map(|v| v.as_str().len()))
        + len(r.rule_name.as_ref().map(|v| v.as_str().len()))
        + len(r.protocol.as_ref().map(|v| v.as_str().len()))
        + len(r.rule_class.as_ref().map(|v| v.as_str().len()))
}

/// The SQL expression for [`estimate_bytes`] over an `events` row.
pub fn row_bytes_sql() -> String {
    let lengths: String = TEXT_COLUMNS
        .iter()
        .map(|c| format!(" + coalesce(length(CAST({c} AS BLOB)), 0)"))
        .collect();
    format!("({ROW_OVERHEAD}{lengths})")
}
