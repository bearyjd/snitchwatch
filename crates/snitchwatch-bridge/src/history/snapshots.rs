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
//! * **Only canonical bytes become text.** A [`SnapshotRecord`] can be built
//!   only here, and [`snapshot_for`] recomputes the hash from the canonical
//!   bytes and refuses an identity whose hash is not theirs.
//! * **Caps** ([`SnapshotLimits`], D7): at most [`MAX_SNAPSHOTS`] snapshots,
//!   [`caps::SNAPSHOT`] bytes each (the type and the schema `CHECK`) and
//!   [`MAX_TOTAL_BYTES`] in all (less if the budget's share is smaller).
//!   Over a bound the **oldest snapshot is dropped even if events still refer
//!   to it**: the event keeps its `rule_hash`, only the display text goes, so
//!   the limit always has a victim. Only a snapshot too large for the total
//!   by itself is refused ([`SnapshotOutcome::Refused`]). The running total
//!   is a [`SnapshotUsage`], read once per transaction and kept up to date.
//! * **Cleanup** ([`delete_oldest_events`]): the events and the snapshots
//!   they leave unreferenced go in one statement sequence on one connection,
//!   so the caller's transaction makes them atomic (a fault before `COMMIT`
//!   leaves both).
//!
//! Dormant: called only by the history writer.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use super::budget::Budget;
use super::caps::RULE_HASH;
use super::caps::{Snapshot, SNAPSHOT};
use super::record::insert_snapshot;
use super::rule_identity::{hash_of, RuleIdentity};
use crate::cache::rules::MAX_OPERATOR_DEPTH;

/// Most snapshots the table holds (D7).
pub const MAX_SNAPSHOTS: u64 = 2000;
/// Most snapshot bytes the table holds (D7): 4 MiB.
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
/// The snapshot table's share of the main file, a ceiling on the above: one
/// tenth.
const TOTAL_SHARE_DIVISOR: u64 = 10;
const VERSION: &[u8] = b"SWRI\x01";

/// Why a snapshot could not be built. Never carries rule text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("the canonical bytes are not a form this build wrote")]
    Malformed,
    #[error("the snapshot is longer than {SNAPSHOT} bytes")]
    TooLarge,
    #[error("the identity's hash is not the hash of its canonical bytes")]
    HashMismatch,
}

/// A `rules_seen` row (D7). Built only by [`snapshot_for`] (and, in tests,
/// [`SnapshotRecord::for_test`]), so its text always comes from canonical
/// bytes whose hash is `rule_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    rule_hash: [u8; RULE_HASH],
    snapshot: Snapshot,
    first_seen_ms: i64,
}

impl SnapshotRecord {
    pub(crate) fn rule_hash(&self) -> [u8; RULE_HASH] {
        self.rule_hash
    }

    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub(crate) fn first_seen_ms(&self) -> i64 {
        self.first_seen_ms
    }

    /// Arbitrary contents, for tests of the storage layers.
    #[cfg(test)]
    pub(crate) fn for_test(
        rule_hash: [u8; RULE_HASH],
        snapshot: Snapshot,
        first_seen_ms: i64,
    ) -> Self {
        Self {
            rule_hash,
            snapshot,
            first_seen_ms,
        }
    }
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

    /// D7's bounds, the byte total lowered to a tenth of the main file when
    /// that is smaller.
    pub fn from_budget(budget: &Budget) -> Self {
        Self::new(
            MAX_SNAPSHOTS,
            MAX_TOTAL_BYTES.min(budget.main_bytes / TOTAL_SHARE_DIVISOR),
        )
    }
}

/// What [`admit_snapshot`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOutcome {
    Stored,
    AlreadyPresent,
    /// Too large for the total by itself (or a zero count cap): not stored,
    /// nothing evicted.
    Refused,
}

/// What the snapshot table holds, read once per transaction and updated as
/// [`admit_snapshot`] stores and evicts, so no admission re-sums the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotUsage {
    count: u64,
    bytes: u64,
}

impl SnapshotUsage {
    pub(crate) fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let (count, bytes): (i64, i64) = conn.query_row(
            "SELECT count(*), coalesce(sum(length(CAST(snapshot AS BLOB))), 0) FROM rules_seen",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(Self {
            count: count.max(0) as u64,
            bytes: bytes.max(0) as u64,
        })
    }
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
    if hash_of(&identity.canonical) != identity.hash {
        return Err(SnapshotError::HashMismatch);
    }
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
        rule_hash: hash_of(&identity.canonical),
        snapshot: Snapshot::new(&text).ok_or(SnapshotError::TooLarge)?,
        first_seen_ms,
    })
}

/// Drops the oldest snapshot, referenced or not (D7); false if the table is
/// empty.
fn evict_oldest(conn: &Connection, usage: &mut SnapshotUsage) -> rusqlite::Result<bool> {
    let victim: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, length(CAST(snapshot AS BLOB)) FROM rules_seen \
             ORDER BY first_seen_ms, id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, len)) = victim else {
        return Ok(false);
    };
    conn.execute("DELETE FROM rules_seen WHERE id = ?1", [id])?;
    usage.count = usage.count.saturating_sub(1);
    usage.bytes = usage.bytes.saturating_sub(len.max(0) as u64);
    Ok(true)
}

/// Stores `record` unless its hash is present, within `limits`: makes room
/// by dropping the oldest snapshots, whether or not events still refer to
/// them (D7). `usage` is the table's usage ([`SnapshotUsage::load`] once per
/// transaction) and is kept current. Run it inside the transaction that
/// inserts the event, so an eviction rolls back with it.
pub fn admit_snapshot(
    conn: &Connection,
    limits: &SnapshotLimits,
    usage: &mut SnapshotUsage,
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
    while usage.count >= limits.max_count
        || usage.bytes.saturating_add(len) > limits.max_total_bytes
    {
        if !evict_oldest(conn, usage)? {
            return Ok(SnapshotOutcome::Refused);
        }
    }
    insert_snapshot(conn, record)?;
    usage.count += 1;
    usage.bytes += len;
    Ok(SnapshotOutcome::Stored)
}

/// Deletes, on `conn`, each snapshot in `hashes` that no remaining event
/// refers to (D7: the one cleanup every deletion path calls, so none can
/// leave a snapshot behind). Returns the snapshot bytes freed.
pub(super) fn delete_unreferenced(conn: &Connection, hashes: &[&[u8]]) -> rusqlite::Result<u64> {
    let mut freed = 0u64;
    let mut seen: Vec<&[u8]> = Vec::new();
    for hash in hashes {
        if seen.contains(hash) {
            continue;
        }
        seen.push(hash);
        let bytes: Option<i64> = conn
            .query_row(
                "SELECT length(CAST(snapshot AS BLOB)) FROM rules_seen WHERE rule_hash = ?1 \
                 AND NOT EXISTS (SELECT 1 FROM events WHERE events.rule_hash = ?1)",
                [hash],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(bytes) = bytes {
            conn.execute("DELETE FROM rules_seen WHERE rule_hash = ?1", [hash])?;
            freed += bytes.max(0) as u64;
        }
    }
    Ok(freed)
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
    let hashes: Vec<&[u8]> = hashes.iter().map(Vec::as_slice).collect();
    delete_unreferenced(conn, &hashes)?;
    Ok(deleted as u64)
}
