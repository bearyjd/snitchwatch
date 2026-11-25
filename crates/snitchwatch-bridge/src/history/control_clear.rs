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

use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::Connection;

use super::capture::Capture;
use super::control::{save_disabled, spawn_capture, ControlGate, Refused};
use super::control_files::{FilesError, Step, StoreDir};
use super::control_preview::{scan_events, Checksum, Token, TokenId};
use super::{open_store, settings, Location, Settings};

/// The most events that a clear carries into the new store.
const CARRY_MAX: i64 = 10_000;
const EVENT_COLUMNS: &str = "ts_ms, decided_ts_ms, process_path, dst_host, dst_ip, dst_port, \
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
        let location = self.location()?.clone();
        let ttl = self.0.limits.token_ttl;
        let token = self
            .0
            .tokens
            .take(id, self.0.generation.current(), ttl)
            .ok_or(Refused::StaleToken)?;
        let guard = self.quiesce()?;
        let capture = self.0.slot.get();
        let prior = settings::load(&location).ok().flatten();
        let saved = match &capture {
            Some(c) => c.close_admission(&guard, &self.0.generation),
            None => guard.advance(&self.0.generation).map(|_| None),
        }
        .map_err(|_| Refused::Unavailable("generation counter exhausted".into()))?;
        if kind == Kind::Delete {
            if let Err(refused) = save_disabled(&location) {
                if let Some(c) = &capture {
                    c.restore_consent(saved);
                }
                return Err(refused);
            }
        }
        self.0.slot.set(None);
        if let Some(c) = &capture {
            c.quiesce_writer(false);
        }
        let done = self
            .remove_files(&location, &token, kind, observe)
            .and_then(|carry| self.finish(&location, (kind, capture.is_some()), carry, observe));
        if let Err(refused) = &done {
            tracing::warn!(%refused, "a control operation was refused; restarting recording");
            self.rollback(&location, prior, capture.is_some());
        }
        drop(guard);
        done
    }

    fn remove_files(
        &self,
        location: &Location,
        token: &Token,
        kind: Kind,
        observe: &dyn Fn(Step),
    ) -> Result<Carry, Refused> {
        let dir = StoreDir::open(location.dir(), super::open::effective_uid()).map_err(files)?;
        dir.check_removable().map_err(files)?;
        let carry = match crate::sqlite_file::open_owner_only_readonly(&location.db_path()) {
            Ok(mut conn) => recheck(&mut conn, token, kind == Kind::Clear)?,
            Err(crate::sqlite_file::OpenError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Carry::default()
            }
            Err(e) => return Err(Refused::Files(e.to_string())),
        };
        dir.remove_store(observe).map_err(files)?;
        Ok(carry)
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

    /// A refusal after the writer stopped: put the settings back and record
    /// again on whatever store is there (a missing one is created empty).
    fn rollback(&self, location: &Location, prior: Option<Settings>, was_recording: bool) {
        if let Some(prior) = prior {
            if let Err(e) = settings::save(location, &prior) {
                tracing::error!(error = %e, "history settings could not be put back");
            }
        }
        if !was_recording {
            return;
        }
        let restarted: Result<Capture, String> = settings::load(location)
            .map_err(|e| e.to_string())
            .and_then(|s| s.ok_or_else(|| "settings are gone".to_owned()))
            .and_then(|s| spawn_capture(location, &s, &self.0));
        match restarted {
            Ok(capture) => self.0.slot.set(Some(Arc::new(capture))),
            Err(reason) => tracing::error!(%reason, "history could not restart recording"),
        }
    }
}

/// With the writer stopped: the previewed rows still add up to the token,
/// and (for clear) what came after them, ready to carry.
fn recheck(conn: &mut Connection, token: &Token, carry: bool) -> Result<Carry, Refused> {
    let scan =
        scan_events(conn, Some(token.max_id), Arc::new(|| false)).map_err(|_| Refused::Changed)?;
    let same: Checksum = scan.checksum;
    if same != token.checksum {
        return Err(Refused::Changed);
    }
    if carry {
        read_carry(conn, token.max_id).map_err(|_| Refused::Changed)
    } else {
        Ok(Carry::default())
    }
}

fn read_carry(conn: &Connection, after: i64) -> rusqlite::Result<Carry> {
    let many: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE id > ?1", [after], |r| {
        r.get(0)
    })?;
    if many > CARRY_MAX {
        return Err(rusqlite::Error::QueryReturnedNoRows);
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
