//! PR 4b: the stored half of D7. A [`RuleIdentity`]'s redacted canonical
//! bytes become a display snapshot (`rules_seen.snapshot`, JSON text); the
//! snapshot table is bounded; and deleting events cleans up the snapshots
//! nothing refers to any more, in the same transaction.
//!
//! * **Nothing outside the identity is stored.** [`snapshot_for`] reads only
//!   `RuleIdentity::canonical` (the bytes the hash covers), never the `Rule`,
//!   so a withheld value, the description and the creation time cannot reach
//!   the file; a withheld operand shows `"withheld": true`, never a value or
//!   a digest of one. A snapshot over [`caps::SNAPSHOT`] bytes after JSON
//!   escaping is refused ([`SnapshotError::TooLarge`]), never cut.
//! * **Caps** ([`SnapshotLimits`]): a count, [`caps::SNAPSHOT`] bytes each
//!   (the type and the schema `CHECK`) and a total. Only snapshots that no
//!   event refers to are ever evicted, oldest first; events are never
//!   touched. When the table is full of referenced snapshots, the new
//!   snapshot is **refused** ([`SnapshotOutcome::Refused`]) and its event is
//!   stored without one: the hash still identifies the rule, the display text
//!   is missing. It fails closed; nothing referenced is dropped.
//! * **Cleanup** ([`delete_oldest_events`]): the events and the snapshots
//!   they leave unreferenced go in one statement sequence on one connection,
//!   so the caller's transaction makes them atomic (a fault before `COMMIT`
//!   leaves both).
//!
//! Dormant: called only by the history writer.

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use super::budget::Budget;
use super::caps::{Snapshot, SNAPSHOT};
use super::record::{insert_snapshot, SnapshotRecord};
use super::rule_identity::RuleIdentity;
use crate::cache::rules::MAX_OPERATOR_DEPTH;

/// Most snapshots the table holds.
pub const MAX_SNAPSHOTS: u64 = 1024;
/// The snapshot table's share of the main file: one tenth.
const TOTAL_SHARE_DIVISOR: u64 = 10;
const VERSION: &[u8] = b"SWRI\x01";

/// Why a snapshot could not be built. Never carries rule text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("the canonical bytes are not a form this build wrote")]
    Malformed,
    #[error("the snapshot is longer than {SNAPSHOT} bytes")]
    TooLarge,
}

/// The snapshot table's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    pub max_count: u64,
    pub max_total_bytes: u64,
}

impl SnapshotLimits {
    pub fn new(max_count: u64, max_total_bytes: u64) -> Self {
        Self {
            max_count,
            max_total_bytes,
        }
    }

    /// At most [`MAX_SNAPSHOTS`], and a tenth of the main file in bytes.
    pub fn from_budget(budget: &Budget) -> Self {
        Self::new(MAX_SNAPSHOTS, budget.main_bytes / TOTAL_SHARE_DIVISOR)
    }
}

/// What [`admit_snapshot`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOutcome {
    Stored,
    AlreadyPresent,
    /// Over a cap with nothing evictable: not stored.
    Refused,
}

/// A byte cursor over the canonical form (`rule_identity::Encoder`'s layout).
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        if self.0.len() < n {
            return Err(SnapshotError::Malformed);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }

    fn flag(&mut self) -> Result<bool, SnapshotError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::Malformed),
        }
    }

    fn u32(&mut self) -> Result<usize, SnapshotError> {
        let b: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| SnapshotError::Malformed)?;
        Ok(u32::from_be_bytes(b) as usize)
    }

    fn text(&mut self) -> Result<&'a str, SnapshotError> {
        let len = self.u32()?;
        std::str::from_utf8(self.take(len)?).map_err(|_| SnapshotError::Malformed)
    }

    fn operator(&mut self, depth: usize) -> Result<Value, SnapshotError> {
        if depth > MAX_OPERATOR_DEPTH {
            return Err(SnapshotError::Malformed);
        }
        let mut node = Map::new();
        node.insert("type".into(), self.text()?.into());
        node.insert("operand".into(), self.text()?.into());
        node.insert("sensitive".into(), self.flag()?.into());
        match self.byte()? {
            1 => {
                node.insert("value".into(), self.text()?.into());
            }
            0 => {
                node.insert("withheld".into(), true.into());
            }
            2 => {}
            _ => return Err(SnapshotError::Malformed),
        }
        let members = self.u32()?;
        // Each member takes at least one byte: no huge allocation from a count.
        if members > self.0.len() {
            return Err(SnapshotError::Malformed);
        }
        let list = (0..members)
            .map(|_| self.operator(depth + 1))
            .collect::<Result<Vec<_>, _>>()?;
        node.insert("members".into(), Value::Array(list));
        Ok(Value::Object(node))
    }
}

/// The display snapshot of `identity`, as the `rules_seen` row to store.
pub fn snapshot_for(
    identity: &RuleIdentity,
    first_seen_ms: i64,
) -> Result<SnapshotRecord, SnapshotError> {
    let mut r = Reader(&identity.canonical);
    if r.take(VERSION.len())? != VERSION {
        return Err(SnapshotError::Malformed);
    }
    let name = r.text()?;
    let action = r.text()?;
    let duration = r.text()?;
    let (enabled, precedence, nolog, has_operator) = (r.flag()?, r.flag()?, r.flag()?, r.flag()?);
    let operator = if has_operator {
        r.operator(0)?
    } else {
        Value::Null
    };
    if !r.0.is_empty() {
        return Err(SnapshotError::Malformed);
    }
    let text = json!({
        "v": 1,
        "name": name,
        "action": action,
        "duration": duration,
        "enabled": enabled,
        "precedence": precedence,
        "nolog": nolog,
        "list_contents_not_recorded": identity.qualifiers.list_contents_not_recorded,
        "withheld_conditions": identity.qualifiers.withheld_conditions,
        "operator": operator,
    })
    .to_string();
    Ok(SnapshotRecord {
        rule_hash: identity.hash,
        snapshot: Snapshot::new(&text).ok_or(SnapshotError::TooLarge)?,
        first_seen_ms,
    })
}

/// Evicts the oldest snapshot that no event refers to; false if none.
fn evict_one_unreferenced(conn: &Connection) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "DELETE FROM rules_seen WHERE id = (SELECT id FROM rules_seen WHERE NOT EXISTS \
         (SELECT 1 FROM events WHERE events.rule_hash = rules_seen.rule_hash) \
         ORDER BY first_seen_ms, id LIMIT 1)",
        [],
    )?;
    Ok(n > 0)
}

fn usage(conn: &Connection) -> rusqlite::Result<(u64, u64)> {
    let (count, bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(length(CAST(snapshot AS BLOB))), 0) FROM rules_seen",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((count.max(0) as u64, bytes.max(0) as u64))
}

/// Stores `record` unless its hash is present, within `limits`: makes room
/// by evicting unreferenced snapshots only, else refuses. Run it inside the
/// transaction that inserts the event, so an eviction rolls back with it.
pub fn admit_snapshot(
    conn: &Connection,
    limits: &SnapshotLimits,
    record: &SnapshotRecord,
) -> rusqlite::Result<SnapshotOutcome> {
    let present: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM rules_seen WHERE rule_hash = ?1)",
        params![record.rule_hash.as_slice()],
        |r| r.get(0),
    )?;
    if present {
        return Ok(SnapshotOutcome::AlreadyPresent);
    }
    let len = record.snapshot.as_str().len() as u64;
    if limits.max_count == 0 || len > limits.max_total_bytes {
        return Ok(SnapshotOutcome::Refused);
    }
    loop {
        let (count, bytes) = usage(conn)?;
        if count < limits.max_count && bytes.saturating_add(len) <= limits.max_total_bytes {
            break;
        }
        if !evict_one_unreferenced(conn)? {
            return Ok(SnapshotOutcome::Refused);
        }
    }
    insert_snapshot(conn, record)?;
    Ok(SnapshotOutcome::Stored)
}

/// Deletes the oldest `n` events and, on the same connection, every snapshot
/// that none of the remaining events refers to. Returns the events deleted.
pub fn delete_oldest_events(conn: &Connection, n: u64) -> rusqlite::Result<u64> {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    let hashes: Vec<Vec<u8>> = conn
        .prepare(
            "SELECT DISTINCT rule_hash FROM \
             (SELECT rule_hash FROM events ORDER BY ts_ms, id LIMIT ?1) \
             WHERE rule_hash IS NOT NULL",
        )?
        .query_map([n], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let deleted = conn.execute(
        "DELETE FROM events WHERE id IN \
         (SELECT id FROM events ORDER BY ts_ms, id LIMIT ?1)",
        [n],
    )?;
    for hash in hashes {
        conn.execute(
            "DELETE FROM rules_seen WHERE rule_hash = ?1 AND NOT EXISTS \
             (SELECT 1 FROM events WHERE events.rule_hash = ?1)",
            [hash],
        )?;
    }
    Ok(deleted as u64)
}
