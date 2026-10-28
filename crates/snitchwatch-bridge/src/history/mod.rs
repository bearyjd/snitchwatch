//! Connection history, PR 1: the dormant store (design
//! `docs/superpowers/specs/2026-10-27-connection-history-design.md`, D2, D3,
//! S1). Nothing here is called from the running bridge yet: no thread, no
//! socket message and no file, until the owner-visible enable action lands in
//! a later PR. Reading the state of a history that was never enabled creates
//! nothing ([`load_state`]).
//!
//! * [`locate`] resolves `<state dir>/history/` per bridge mode and never
//!   falls back to another path.
//! * [`open`] validates the directory (exactly 0700, ours, not a link) and
//!   opens `history.db` through [`crate::sqlite_file::open_owner_only`].
//! * [`schema`] pins `page_size`, checks a real `PRAGMA user_version`.
//! * [`caps`] and [`record`] are S1: the field-cap table and the insert API.
//! * [`settings`] is the bridge-owned enabled/consent/retention file.

pub mod caps;
pub mod open;
pub mod record;
pub mod schema;
pub mod settings;

use std::path::{Path, PathBuf};

pub use open::Store;
pub use settings::{Consent, Settings};

/// The history directory, inside the bridge's state directory.
pub const DIR_NAME: &str = "history";
pub const DB_FILE: &str = "history.db";
pub const SETTINGS_FILE: &str = "settings.json";
/// The only state directory the system bridge accepts (`StateDirectory=` of
/// `snitchwatch-system-bridge.service`); the same constant the bridge's
/// storage resolution uses.
pub const SYSTEM_STATE_DIR: &str = "/var/lib/snitchwatch";

/// Which bridge this is (the CLI's `BridgeMode`, without the dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    User,
    System,
}

/// Why history can't be used. A plain sentence, shown as
/// `unavailable: <reason>`. Never a reason to try another location.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Unavailable(pub String);

impl Unavailable {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// Where history lives for one bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    dir: PathBuf,
}

impl Location {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir.join(DB_FILE)
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }
}

/// The history directory for a bridge whose resolved state directory is
/// `state_dir` (`None`: it has none, i.e. `Storage::Ephemeral`). The system
/// bridge accepts only [`SYSTEM_STATE_DIR`]; the per-user bridge any absolute
/// directory. Anything else is `unavailable`; there is no second choice.
pub fn locate(mode: Mode, state_dir: Option<&Path>) -> Result<Location, Unavailable> {
    let Some(state_dir) = state_dir else {
        return Err(Unavailable::new("no persistent state directory"));
    };
    if !state_dir.is_absolute() {
        return Err(Unavailable::new(
            "the state directory is not an absolute path",
        ));
    }
    if mode == Mode::System && state_dir != Path::new(SYSTEM_STATE_DIR) {
        return Err(Unavailable::new(format!(
            "the system bridge's history lives under {SYSTEM_STATE_DIR}, not {}",
            state_dir.display()
        )));
    }
    Ok(Location {
        dir: state_dir.join(DIR_NAME),
    })
}

/// What history is doing, as read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Never enabled, or disabled. Nothing is recorded.
    Off,
    Enabled(Settings),
    Unavailable(Unavailable),
}

/// Read the state. **Creates nothing**: no directory, no settings file.
pub fn load_state(location: &Location) -> State {
    load_state_as(location, open::effective_uid())
}

pub(crate) fn load_state_as(location: &Location, euid: u32) -> State {
    match open::dir_exists(location.dir()) {
        Ok(false) => return State::Off,
        Ok(true) => {}
        Err(reason) => return State::Unavailable(reason),
    }
    if let Err(reason) = open::check_dir(location.dir(), euid) {
        return State::Unavailable(reason);
    }
    match settings::load(location) {
        Ok(Some(settings)) if settings.enabled => State::Enabled(settings),
        Ok(_) => State::Off,
        Err(reason) => State::Unavailable(reason),
    }
}

/// The user's enable action: create the history directory (0700) if needed
/// and write the settings file with `enabled` and the consent record. The
/// only way `enabled` becomes true; it cannot be set without consent.
pub fn enable(location: &Location, consent: Consent) -> Result<Settings, Unavailable> {
    enable_as(location, consent, open::effective_uid())
}

pub(crate) fn enable_as(
    location: &Location,
    consent: Consent,
    euid: u32,
) -> Result<Settings, Unavailable> {
    consent.validate()?;
    open::ensure_dir(location.dir(), euid)?;
    let current = settings::load(location)?.unwrap_or_default();
    let next = Settings {
        enabled: true,
        consent: Some(consent),
        ..current
    };
    settings::save(location, &next)?;
    Ok(next)
}

/// Open (creating on first use) `history.db`, only while history is enabled.
pub fn open_store(location: &Location) -> Result<Store, Unavailable> {
    open_store_as(location, open::effective_uid())
}

pub(crate) fn open_store_as(location: &Location, euid: u32) -> Result<Store, Unavailable> {
    match load_state_as(location, euid) {
        State::Enabled(_) => Store::open(location, euid),
        State::Off => Err(Unavailable::new("history is off")),
        State::Unavailable(reason) => Err(reason),
    }
}

#[cfg(test)]
mod caps_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
