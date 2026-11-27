//! PR 10: clear and delete. Both run the gate's order (see [`super::control`])
//! and then the file work: a store is never emptied in place. A refusal
//! with the old store still whole restarts the writer on it.
//!
//! * **clear** keeps `settings.json` and consent and goes on recording if it
//!   was recording. What was accepted after the preview is **kept** (owner
//!   decision 2): the new store is built with it beside the old one, synced,
//!   and renamed over it (#444). From that rename on, the clear is never
//!   undone. A clear that is not recording and has nothing to keep removes
//!   the store as delete does.
//! * **delete** renames the store to a tombstone, unlinks it and removes
//!   `settings.json`: nothing is kept, history is off and needs a fresh
//!   consent to start again.
//!
//! The prompts pending at the time are discarded with the old store (owner
//! decision 5): no row, no gap, their pins go with the old registry.

use std::cell::Cell;
use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::Connection;

use super::budget::Budget;
use super::control::{persist_off, restore_settings, spawn_capture, ControlGate, Refused};
use super::control_files::{FileId, FilesError, Replaced, Step, StoreDir, STAGED};
use super::control_preview::{recheck_tombstone, scan_events, Checksum, ScanError, Token, TokenId};
use super::{schema, settings, Location, Settings, DB_FILE};

/// `PRAGMA synchronous = FULL`.
const SYNCHRONOUS_FULL: i64 = 2;

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
        if kind == Kind::Clear {
            return self.clear_files(location, token, was_recording, observe);
        }
        // Once the store is out of place the user's request cannot be undone.
        let reached = Cell::new((false, false));
        let watch = |step: Step| {
            let (renamed, unlinked) = reached.get();
            reached.set((
                renamed || step == Step::Renamed,
                unlinked || step == Step::TombstoneRemoved,
            ));
            observe(step);
        };
        let done = previewed(location, token, false)
            .and_then(|(dir, _)| dir.remove_store(&watch).map_err(files))
            .and_then(|()| remove_settings(location));
        match done {
            Err(refused) if reached.get().0 => {
                // Delete means off: settings were saved off (consent revoked)
                // before anything closed, so keep them, respawn nothing and
                // say what failed.
                tracing::error!(%refused, "a delete failed after the store was removed");
                Err(incomplete(reached.get().1, &refused))
            }
            Err(refused) => Err(self.undo(location, changed, was_recording, refused)),
            ok => ok,
        }
    }

    /// Clear: with rows to keep or recording to resume, the store is replaced
    /// by a staged one; otherwise there is nothing to make and it is removed.
    fn clear_files(
        &self,
        location: &Location,
        token: &Token,
        was_recording: bool,
        observe: &dyn Fn(Step),
    ) -> Result<(), Refused> {
        let (dir, found) = match previewed(location, token, true) {
            Ok(checked) => checked,
            Err(refused) => return Err(self.undo(location, None, was_recording, refused)),
        };
        // With no store there is nothing the staged one may replace.
        let had_store = found.is_some();
        let carry = found.unwrap_or_default();
        if !was_recording && carry.events.is_empty() {
            return remove_unrecorded(&dir, observe);
        }
        observe(Step::Begin);
        let replaced = staging_settings(location, was_recording).and_then(|settings| {
            let id = stage(&dir, location, &settings, &carry, observe)?;
            let replaced = dir.replace_store(id, had_store, observe).map_err(files)?;
            Ok((settings, replaced))
        });
        match replaced {
            Ok((settings, replaced)) => {
                let resume = was_recording.then_some(&settings);
                self.after_replace(location, replaced, resume, carry.events.len())
            }
            // Nothing was replaced.
            Err(refused) => {
                dir.discard_staged();
                Err(self.undo(location, None, was_recording, refused))
            }
        }
    }

    /// The rename was attempted and may have happened: nothing is undone.
    /// `resume` holds the settings to record with, if it was recording;
    /// `kept` is how many rows the new store holds.
    fn after_replace(
        &self,
        location: &Location,
        replaced: Replaced,
        resume: Option<&Settings>,
        kept: usize,
    ) -> Result<(), Refused> {
        let off = if resume.is_some() {
            "; recording stays off"
        } else {
            ""
        };
        let linked = match replaced {
            Replaced::Done { linked } => linked,
            Replaced::NotDurable { linked, reason } => {
                tracing::error!(%reason, "a cleared history store could not be made durable");
                let link = if linked {
                    format!("; {}", FilesError::Linked(DB_FILE))
                } else {
                    String::new()
                };
                return Err(Refused::ClearedNotDurable(format!("{reason}{link}{off}")));
            }
            Replaced::Unknown(reason) => {
                tracing::error!(%reason, "the outcome of a clear is unknown");
                return Err(Refused::Unavailable(format!(
                    "cannot tell whether the history was cleared: {reason}{off}"
                )));
            }
        };
        let resumed = resume.map(|settings| self.record_again(location, settings));
        if linked {
            return Err(linked_clear(resumed, off));
        }
        match resumed {
            Some(Err(reason)) => Err(Refused::ClearedNotResumed { kept, reason }),
            _ => Ok(()),
        }
    }

    /// Records on the replaced store, with one retry: the clear cannot be
    /// undone, so a passing failure is worth a second try.
    fn record_again(&self, location: &Location, settings: &Settings) -> Result<(), String> {
        let capture = spawn_capture(location, settings, &self.0).or_else(|first| {
            tracing::warn!(reason = %first, "recording did not resume; trying once more");
            spawn_capture(location, settings, &self.0)
        })?;
        self.0.slot.set(Some(Arc::new(capture)));
        Ok(())
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

/// With the writer stopped, before anything changes: the files can be
/// removed and the previewed rows still add up to the token. With `carry`,
/// also what came after them; `None` when there is no store.
fn previewed(
    location: &Location,
    token: &Token,
    carry: bool,
) -> Result<(StoreDir, Option<Carry>), Refused> {
    let dir = StoreDir::open(location.dir(), super::open::effective_uid()).map_err(files)?;
    dir.check_removable().map_err(files)?;
    let found = match crate::sqlite_file::open_owner_only_readonly(&location.db_path()) {
        Ok(mut conn) => Some(recheck(&mut conn, token, carry, None)?),
        Err(crate::sqlite_file::OpenError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            None
        }
        Err(e) => return Err(Refused::Files(e.to_string())),
    };
    Ok((dir, found))
}

fn remove_settings(location: &Location) -> Result<(), Refused> {
    StoreDir::open(location.dir(), super::open::effective_uid())
        .and_then(|dir| dir.remove_settings())
        .map_err(files)
}

/// A clear that is not recording and keeps nothing: the store is removed.
/// Past the rename its rows are still on disk if this fails.
fn remove_unrecorded(dir: &StoreDir, observe: &dyn Fn(Step)) -> Result<(), Refused> {
    let renamed = Cell::new(false);
    let removed = dir.remove_store(&|step| {
        renamed.set(renamed.get() || step == Step::Renamed);
        observe(step);
    });
    removed.map_err(|e| {
        let refused = files(e);
        if renamed.get() {
            tracing::error!(%refused, "a clear failed after the store was removed");
            Refused::DataLeft(refused.to_string())
        } else {
            refused
        }
    })
}

/// A clear that replaced the store while another name still keeps the old
/// one. `resumed` is how resuming went, if it was recording.
fn linked_clear(resumed: Option<Result<(), String>>, off: &str) -> Refused {
    let recording = match resumed {
        None => String::new(),
        Some(Ok(())) => "; recording resumed on a new store".into(),
        Some(Err(reason)) => format!("; recording could not be resumed: {reason}{off}"),
    };
    let link = FilesError::Linked(DB_FILE);
    Refused::DataLeft(format!("{link}{recording}"))
}

/// The settings a staged store is built under. Without recording to resume,
/// unreadable ones do not stop the rows from being kept.
fn staging_settings(location: &Location, was_recording: bool) -> Result<Settings, Refused> {
    match settings::load(location) {
        Ok(Some(settings)) => Ok(settings),
        _ if !was_recording => Ok(Settings::default()),
        _ => Err(Refused::Settings("settings are gone".into())),
    }
}

/// Builds the store that will replace the old one: a file created through the
/// directory handle, the carry committed with `synchronous = FULL`, checked,
/// closed and synced. The old store is not touched.
fn stage(
    dir: &StoreDir,
    location: &Location,
    settings: &Settings,
    carry: &Carry,
    observe: &dyn Fn(Step),
) -> Result<FileId, Refused> {
    let id = dir.create_staged().map_err(files)?;
    let budget = Budget::new(u64::from(settings.budget_mib) * 1024 * 1024)
        .map_err(|e| Refused::Unavailable(e.to_string()))?;
    let mut conn = schema::prepare(&location.dir().join(STAGED), &budget)
        .map_err(|e| Refused::Unavailable(e.to_string()))?;
    dir.require_staged(id).map_err(files)?;
    observe(Step::Created);
    insert_carry(&mut conn, carry).map_err(Refused::Unavailable)?;
    observe(Step::Carried);
    check_staged(&conn, carry).map_err(Refused::Unavailable)?;
    conn.close()
        .map_err(|(_, e)| Refused::Unavailable(e.to_string()))?;
    dir.sync_staged(id).map_err(files)?;
    observe(Step::Staged);
    Ok(id)
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

/// One durable transaction on the staged store.
fn insert_carry(conn: &mut Connection, carry: &Carry) -> Result<(), String> {
    conn.execute_batch("PRAGMA synchronous = FULL;")
        .map_err(|e| e.to_string())?;
    if pragma(conn, "synchronous")? != SYNCHRONOUS_FULL {
        return Err("synchronous = FULL could not be set".into());
    }
    if carry.events.is_empty() {
        return Ok(());
    }
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

fn pragma(conn: &Connection, name: &str) -> Result<i64, String> {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// The staged store is at this build's version and holds exactly the carry.
fn check_staged(conn: &Connection, carry: &Carry) -> Result<(), String> {
    let version = pragma(conn, "user_version")?;
    if version != schema::SCHEMA_VERSION {
        return Err(format!("the staged store is at version {version}"));
    }
    for (table, kept) in [
        ("events", carry.events.len()),
        ("rules_seen", carry.rules.len()),
    ] {
        let held: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if usize::try_from(held) != Ok(kept) {
            return Err(format!(
                "the staged store holds {held} rows in {table}, not the {kept} to keep"
            ));
        }
    }
    Ok(())
}
