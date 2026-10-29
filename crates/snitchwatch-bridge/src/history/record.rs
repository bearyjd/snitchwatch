//! The insert API (D4): it takes exactly the allowlisted fields, and only in
//! forms that cannot exceed their S1 cap (capped strings, enums, ports,
//! parsed addresses, a fixed-size hash). `process_args`, the environment, the
//! command line and `uid` have no field here, so no caller can store them.

use std::net::IpAddr;

use rusqlite::{params, Connection};

use super::caps::{DstHost, ProcessPath, Protocol, RuleClass, RuleName, Snapshot, RULE_HASH};

/// `events.action`, as stored.
pub(crate) const ACTIONS: [&str; 6] = [
    "allow",
    "deny",
    "blocklist",
    "default-allow",
    "default-deny",
    "pending-unanswered",
];

/// `events.answered_by`, as stored (D4's classification table).
pub(crate) const ANSWERED_BY: [&str; 6] = [
    "rule",
    "default",
    "filter-paused",
    "no-answer",
    "gui",
    "unknown",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Deny,
    Blocklist,
    DefaultAllow,
    DefaultDeny,
    PendingUnanswered,
}

impl Action {
    fn as_str(self) -> &'static str {
        ACTIONS[self as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsweredBy {
    Rule,
    Default,
    FilterPaused,
    NoAnswer,
    Gui,
    Unknown,
}

impl AnsweredBy {
    fn as_str(self) -> &'static str {
        ANSWERED_BY[self as usize]
    }
}

/// One history event. Construct with [`EventRecord::new`] and set the
/// optional fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub ts_ms: i64,
    pub decided_ts_ms: Option<i64>,
    pub process_path: Option<ProcessPath>,
    pub dst_host: Option<DstHost>,
    pub dst_ip: Option<IpAddr>,
    pub dst_port: Option<u16>,
    pub protocol: Option<Protocol>,
    pub action: Action,
    pub answered_by: AnsweredBy,
    pub rule_name: Option<RuleName>,
    pub rule_hash: Option<[u8; RULE_HASH]>,
    pub rule_class: Option<RuleClass>,
    /// At least 1 (the schema refuses 0).
    pub repeat_count: u32,
    pub src_port: Option<u16>,
    pub src_ip: Option<IpAddr>,
}

impl EventRecord {
    pub fn new(ts_ms: i64, action: Action, answered_by: AnsweredBy) -> Self {
        Self {
            ts_ms,
            decided_ts_ms: None,
            process_path: None,
            dst_host: None,
            dst_ip: None,
            dst_port: None,
            protocol: None,
            action,
            answered_by,
            rule_name: None,
            rule_hash: None,
            rule_class: None,
            repeat_count: 1,
            src_port: None,
            src_ip: None,
        }
    }
}

/// A `rules_seen` row (D7). PR 4 builds the redacted snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    pub rule_hash: [u8; RULE_HASH],
    pub snapshot: Snapshot,
    pub first_seen_ms: i64,
}

fn ip_text(ip: Option<IpAddr>) -> Option<String> {
    ip.map(|ip| ip.to_string())
}

/// Insert one event; the new row's id.
pub fn insert_event(conn: &Connection, event: &EventRecord) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO events (ts_ms, decided_ts_ms, process_path, dst_host, dst_ip, dst_port, \
         protocol, action, answered_by, rule_name, rule_hash, rule_class, repeat_count, \
         src_port, src_ip) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            event.ts_ms,
            event.decided_ts_ms,
            event.process_path.as_ref().map(|v| v.as_str()),
            event.dst_host.as_ref().map(|v| v.as_str()),
            ip_text(event.dst_ip),
            event.dst_port,
            event.protocol.as_ref().map(|v| v.as_str()),
            event.action.as_str(),
            event.answered_by.as_str(),
            event.rule_name.as_ref().map(|v| v.as_str()),
            event.rule_hash.as_ref().map(|h| h.as_slice()),
            event.rule_class.as_ref().map(|v| v.as_str()),
            event.repeat_count,
            event.src_port,
            ip_text(event.src_ip),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Insert one rule snapshot. A hash already stored is an error (the unique
/// index); the caller decides whether that is a no-op.
pub fn insert_snapshot(conn: &Connection, snapshot: &SnapshotRecord) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO rules_seen (rule_hash, snapshot, first_seen_ms) VALUES (?1, ?2, ?3)",
        params![
            snapshot.rule_hash.as_slice(),
            snapshot.snapshot.as_str(),
            snapshot.first_seen_ms
        ],
    )?;
    Ok(())
}

/// [`insert_snapshot`] unless that hash is already stored (then nothing is
/// written, so the page budget is not spent).
pub fn ensure_snapshot(conn: &Connection, snapshot: &SnapshotRecord) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO rules_seen (rule_hash, snapshot, first_seen_ms) \
         VALUES (?1, ?2, ?3)",
        params![
            snapshot.rule_hash.as_slice(),
            snapshot.snapshot.as_str(),
            snapshot.first_seen_ms
        ],
    )?;
    Ok(())
}
