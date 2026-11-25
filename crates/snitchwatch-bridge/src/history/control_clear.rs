//! PR 10: clear and delete. Both run the gate's order (see [`super::control`])
//! and then the file work: the store is renamed to a tombstone and
//! unlinked, never emptied in place. A refusal after the writer stopped
//! restarts it on the same store.
//!
//! * **clear** keeps `settings.json` and consent, recreates an empty store
//!   and goes on recording if it was recording. What was accepted after the
//!   preview is **kept**: copied into the new store (owner decision 2).
//! * **delete** also removes `settings.json`: history is off and needs a
//!   fresh consent to start again.
//!
//! The prompts pending at the time are discarded with the old store (owner
//! decision 5): no row, no gap, their pins go with the old registry.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::Connection;

use super::control::{persist_off, restore_settings, spawn_capture, ControlGate, Refused};
use super::control_files::{FilesError, Step, StoreDir};
use super::control_preview::{recheck_tombstone, scan_events, Checksum, ScanError, Token, TokenId};
use super::{open_store, settings, Location, Settings};

/// The most events that a clear carries into the new store.
const CARRY_MAX: i64 = 10_000;
pub(super) const EVENT_COLUMNS: &str =
    "ts_ms, decided_ts_ms, process_path, dst_host, dst_ip, dst_port, \
    protocol, action, answered_by, rule_name, rule_hash, rule_class, repeat_count, src_port, \
    src_ip, prompt_outcome";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Clear,
    Delete,
}

/// What was accepted after the preview, to put in the new store.
#[derive(Default)]
struct Carry {
    events: Vec<Vec<Value>>,
    rules: Vec<(Vec<u8>, String, i64)>,
}

/// A delete that failed past the rename: the rows are still on disk if the
/// tombstone was not unlinked, otherwise only the settings are left.
fn incomplete(tombstone_removed: bool, refused: &Refused) -> Refused {
    if tombstone_removed {
        Refused::Incomplete(format!("settings could not be removed: {refused}"))
    } else {
        Refused::DataLeft(format!("{refused}; recording stays off"))
    }
}

/// A retry that could not finish a leftover: the rows are still on disk when
/// the tombstone is, or when another link keeps them. Nothing here knows
/// whether recording is on, so it says nothing about it.
fn leftover_failed(location: &Location, e: FilesError) -> Refused {
    let tombstone = location.dir().join(super::control_files::TOMBSTONE);
    if matches!(e, FilesError::Linked(_)) || tombstone.symlink_metadata().is_ok() {
        Refused::DataLeft(e.to_string())
    } else {
        files(e)
    }
}

fn files(e: FilesError) -> Refused {
    Refused::Files(e.to_string())
}

impl ControlGate {
    /// Removes all stored history, keeping settings and consent.
    pub fn clear(&self, token: TokenId) -> Result<(), Refused> {
        self.destructive(token, Kind::Clear, &|_| {})
    }

    /// [`clear`](Self::clear) reporting each file step, so a test can kill
    /// the process at exactly that point.
    #[cfg(test)]
    pub(super) fn clear_observed(
        &self,
        token: TokenId,
        observe: &dyn Fn(Step),
    ) -> Result<(), Refused> {
        self.destructive(token, Kind::Clear, observe)
    }

    /// [`delete`](Self::delete) reporting each file step.
    #[cfg(test)]
    pub(super) fn delete_observed(
        &self,
        token: TokenId,
        observe: &dyn Fn(Step),
    ) -> Result<(), Refused> {
        self.destructive(token, Kind::Delete, observe)
    }

    /// Removes the store and its settings; history is off afterwards.
    pub fn delete(&self, token: TokenId) -> Result<(), Refused> {
        self.destructive(token, Kind::Delete, &|_| {})
    }

    fn destructive(&self, id: TokenId, kind: Kind, observe: &dyn Fn(Step)) -> Result<(), Refused> {
        let _op = self.begin()?;
        let location = self.owned_location()?;
        let ttl = self.0.limits.token_ttl;
        let token = self
            .0
            .tokens
            .take(id, self.0.generation.current(), ttl)
            .ok_or(Refused::StaleToken)?;
        let guard = self.quiesce()?;
        self.check_owner()?;
        // What an earlier removal left (a tombstone that could not be
        // unlinked) is finished first, so a retry completes it.
        recheck_tombstone(&location, &token)?;
        StoreDir::open(location.dir(), super::open::effective_uid())
            .and_then(|dir| dir.finish_leftover())
            .map_err(|e| leftover_failed(&location, e))?;
        let capture = self.0.slot.get();
        // Delete turns recording off first, and revokes consent in the same
        // write; nothing here is closed yet, so a refusal changes nothing.
        let changed = match kind {
            Kind::Delete => persist_off(&location, true, true)?,
            Kind::Clear => None,
        };
        if let Err(refused) = self.close(&guard, capture.as_ref()) {
            restore_settings(&location, changed);
            return Err(refused);
        }
        self.0.slot.set(None);
        if let Some(c) = &capture {
            c.quiesce_writer(false);
        }
        let outcome = self.remove_and_finish(
            &location,
            &token,
            kind,
            (changed, capture.is_some()),
            observe,
        );
        drop(guard);
        outcome
    }

    /// With the writer stopped: the file work, then what the outcome means.
    fn remove_and_finish(
        &self,
        location: &Location,
        token: &Token,
        kind: Kind,
        (changed, was_recording): (Option<Settings>, bool),
        observe: &dyn Fn(Step),
    ) -> Result<(), Refused> {
        // Once the store is out of place the user's request cannot be undone.
        let reached = Cell::new((false, false));
        let kept = RefCell::new(Carry::default());
        let watch = |step: Step| {
            let (renamed, unlinked) = reached.get();
            reached.set((
                renamed || step == Step::Renamed,
                unlinked || step == Step::TombstoneRemoved,
            ));
            observe(step);
        };
        let done = self
            .remove_files(location, token, kind, &kept, &watch)
            .and_then(|()| self.finish(location, (kind, was_recording), kept.take(), &watch));
        match done {
            Err(refused) if kind == Kind::Delete && reached.get().0 => {
                // Delete means off: settings were saved off (consent revoked)
                // before anything closed, so keep them, respawn nothing and
                // say what failed.
                tracing::error!(%refused, "a delete failed after the store was removed");
                Err(incomplete(reached.get().1, &refused))
            }
            Err(refused) if kind == Kind::Clear && reached.get() == (true, false) => {
                // The rows are still in the tombstone; the ones accepted
                // after the preview go into a new store (decision 2).
                tracing::error!(%refused, "a clear failed after the store was removed");
                Err(self.keep_rows(location, was_recording, &kept.take(), &refused))
            }
            Err(refused) => Err(self.undo(location, changed, was_recording, refused)),
            ok => ok,
        }
    }

    /// A refusal after the writer stopped: restart recording and say so if
    /// that failed too, instead of claiming that nothing changed.
    fn undo(
        &self,
        location: &Location,
        changed: Option<Settings>,
        was_recording: bool,
        refused: Refused,
    ) -> Refused {
        tracing::warn!(%refused, "a control operation was refused; restarting recording");
        match self.rollback(location, changed, was_recording) {
            Ok(()) => refused,
            Err(reason) => Refused::RollbackFailed {
                refused: refused.to_string(),
                reason,
            },
        }
    }

    /// Leaves what a clear must carry in `kept` before the store is moved,
    /// so a failure past the rename can still write it.
    fn remove_files(
        &self,
        location: &Location,
        token: &Token,
        kind: Kind,
        kept: &RefCell<Carry>,
        observe: &dyn Fn(Step),
    ) -> Result<(), Refused> {
        let dir = StoreDir::open(location.dir(), super::open::effective_uid()).map_err(files)?;
        dir.check_removable().map_err(files)?;
        let carry = match crate::sqlite_file::open_owner_only_readonly(&location.db_path()) {
            Ok(mut conn) => recheck(&mut conn, token, kind == Kind::Clear, None)?,
            Err(crate::sqlite_file::OpenError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Carry::default()
            }
            Err(e) => return Err(Refused::Files(e.to_string())),
        };
        kept.replace(carry);
        dir.remove_store(observe).map_err(files)
    }

    /// After the files are gone: delete drops the settings; clear starts the
    /// new store (with what was carried) if it was recording.
    fn finish(
        &self,
        location: &Location,
        (kind, was_recording): (Kind, bool),
        carry: Carry,
        observe: &dyn Fn(Step),
    ) -> Result<(), Refused> {
        let dir = StoreDir::open(location.dir(), super::open::effective_uid()).map_err(files)?;
        if kind == Kind::Delete {
            return dir.remove_settings().map_err(files);
        }
        if !was_recording {
            return Ok(());
        }
        let settings = settings::load(location)
            .ok()
            .flatten()
            .ok_or_else(|| Refused::Settings("settings are gone".into()))?;
        open_store(location).map_err(|e| Refused::Unavailable(e.to_string()))?;
        observe(Step::Created);
        insert_carry(location, &carry).map_err(|e| Refused::Unavailable(e.to_string()))?;
        observe(Step::Carried);
        let capture = spawn_capture(location, &settings, &self.0).map_err(Refused::Unavailable)?;
        self.0.slot.set(Some(Arc::new(capture)));
        Ok(())
    }

    /// A clear that failed past the rename (the tombstone still holds the
    /// rows): record on a new store with what was carried if it was
    /// recording, and say the rows are on disk. If that fails too, recording
    /// stays off.
    fn keep_rows(
        &self,
        location: &Location,
        was_recording: bool,
        carry: &Carry,
        refused: &Refused,
    ) -> Refused {
        match self.resume_with(location, was_recording, carry) {
            Ok(()) => Refused::DataLeft(format!("{refused}; recording resumed on a new store")),
            Err(reason) => Refused::DataLeft(format!(
                "{refused}; recording could not be resumed: {reason}; recording stays off"
            )),
        }
    }

    fn resume_with(
        &self,
        location: &Location,
        was_recording: bool,
        carry: &Carry,
    ) -> Result<(), String> {
        if !was_recording {
            return Ok(());
        }
        let settings = settings::load(location)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "settings are gone".to_owned())?;
        open_store(location).map_err(|e| e.to_string())?;
        insert_carry(location, carry)?;
        let capture = spawn_capture(location, &settings, &self.0)?;
        self.0.slot.set(Some(Arc::new(capture)));
        Ok(())
    }

    /// Put the settings back and record again on whatever store is there (a
    /// missing one is created empty).
    fn rollback(
        &self,
        location: &Location,
        changed: Option<Settings>,
        was_recording: bool,
    ) -> Result<(), String> {
        if let Some(prior) = changed {
            settings::save(location, &prior).map_err(|e| format!("settings: {e}"))?;
        }
        if !was_recording {
            return Ok(());
        }
        let settings = settings::load(location)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "settings are gone".to_owned())?;
        let capture = spawn_capture(location, &settings, &self.0)?;
        self.0.slot.set(Some(Arc::new(capture)));
        Ok(())
    }
}

/// With the writer stopped: the previewed rows still add up to the token,
/// and (for clear) what came after them, ready to carry.
fn recheck(
    conn: &mut Connection,
    token: &Token,
    carry: bool,
    scan_budget: Option<std::time::Duration>,
) -> Result<Carry, Refused> {
    let scan = scan_events(conn, Some(token.max_id), Arc::new(|| false), scan_budget).map_err(
        |e| match e {
            ScanError::Cancelled => Refused::Busy,
            ScanError::Failed(reason) => Refused::Unavailable(reason),
        },
    )?;
    let same: Checksum = scan.checksum;
    if same != token.checksum {
        return Err(Refused::Changed);
    }
    if carry {
        read_carry(conn, token.max_id)
    } else {
        Ok(Carry::default())
    }
}

fn read_carry(conn: &Connection, after: i64) -> Result<Carry, Refused> {
    read_carry_rows(conn, after).map_err(|e| match e {
        CarryError::TooMany => Refused::TooManyCarried,
        CarryError::Sql(e) => Refused::Unavailable(e.to_string()),
    })
}

enum CarryError {
    TooMany,
    Sql(rusqlite::Error),
}

impl From<rusqlite::Error> for CarryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

fn read_carry_rows(conn: &Connection, after: i64) -> Result<Carry, CarryError> {
    let many: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE id > ?1", [after], |r| {
        r.get(0)
    })?;
    if many > CARRY_MAX {
        return Err(CarryError::TooMany);
    }
    let width = EVENT_COLUMNS.split(',').count();
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM events WHERE id > ?1 ORDER BY id"
    ))?;
    let events = stmt
        .query_map([after], |r| {
            (0..width).map(|i| r.get::<_, Value>(i)).collect()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT rule_hash, snapshot, first_seen_ms FROM rules_seen WHERE rule_hash IN \
         (SELECT rule_hash FROM events WHERE id > ?1)",
    )?;
    let rules = stmt
        .query_map([after], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Carry { events, rules })
}

/// One transaction on the new store, before its writer starts.
fn insert_carry(location: &Location, carry: &Carry) -> Result<(), String> {
    if carry.events.is_empty() {
        return Ok(());
    }
    let mut conn =
        crate::sqlite_file::open_owner_only(&location.db_path()).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (hash, snapshot, first_seen) in &carry.rules {
        tx.execute(
            "INSERT OR IGNORE INTO rules_seen (rule_hash, snapshot, first_seen_ms) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![hash, snapshot, first_seen],
        )
        .map_err(|e| e.to_string())?;
    }
    let marks: Vec<String> = (1..=carry.events[0].len())
        .map(|i| format!("?{i}"))
        .collect();
    let insert = format!(
        "INSERT INTO events ({EVENT_COLUMNS}) VALUES ({})",
        marks.join(", ")
    );
    for row in &carry.events {
        tx.execute(&insert, rusqlite::params_from_iter(row))
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}
