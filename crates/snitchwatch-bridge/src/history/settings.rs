//! The bridge-owned settings file (`<history>/settings.json`, 0600): the
//! enabled flag, retention, budget and the consent record. It lives beside
//! `history.db`, not in it, so clearing the database (which recreates the
//! file) keeps it, and the bridge never records on a stale GUI preference:
//! `enabled` is only ever set by [`super::enable`], with a consent record.
//!
//! It is inside the disk budget (S3), so it has a known maximum: every field
//! is fixed-width or bounded and ASCII-only (no JSON escaping can lengthen
//! it), and the **encoded bytes** are what is checked against
//! [`SETTINGS_MAX_BYTES`], on write and on read. A write replaces the file
//! atomically (temp file, then rename), so for a moment the file and its
//! replacement both exist: the peak is [`SETTINGS_PEAK_RESERVE_BYTES`], which
//! PR 2 takes out of the main file's share of B.

use serde::{Deserialize, Serialize};

use super::{Location, Unavailable};

/// Largest encoded settings file. The worst valid value is about 170 bytes
/// (a test pins it); the rest is headroom for a field added later.
pub const SETTINGS_MAX_BYTES: u64 = 512;
/// The file and its atomic replacement, each rounded up to one 4096-byte
/// filesystem block (the logical maximum fits in one).
pub const SETTINGS_PEAK_RESERVE_BYTES: u64 = 2 * 4096;
// The file and its atomic replacement exist together (checked at compile time).
const _: () = assert!(2 * SETTINGS_MAX_BYTES <= SETTINGS_PEAK_RESERVE_BYTES);
/// Longest consent text identifier (the version of the opt-in text shown).
pub const TEXT_ID_MAX: usize = 64;

pub const FORMAT_VERSION: u32 = 1;
pub const RETENTION_DAYS: [u16; 3] = [7, 30, 90];
pub const BUDGET_MIB: [u16; 3] = [16, 64, 256];
pub const DEFAULT_RETENTION_DAYS: u16 = 30;
pub const DEFAULT_BUDGET_MIB: u16 = 64;

/// What the user agreed to: which opt-in text (by identifier) and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Consent {
    pub text_id: String,
    pub accepted_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    enabled: bool,
    retention_days: u16,
    budget_mib: u16,
    consent: Option<Consent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub retention_days: u16,
    pub budget_mib: u16,
    pub consent: Option<Consent>,
}

impl Default for Settings {
    /// Off, with the default retention and budget and no consent.
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: DEFAULT_RETENTION_DAYS,
            budget_mib: DEFAULT_BUDGET_MIB,
            consent: None,
        }
    }
}

fn bad(reason: impl std::fmt::Display) -> Unavailable {
    Unavailable::new(format!("history settings: {reason}"))
}

impl Consent {
    pub(crate) fn validate(&self) -> Result<(), Unavailable> {
        let ok = !self.text_id.is_empty()
            && self.text_id.len() <= TEXT_ID_MAX
            && self
                .text_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if ok {
            Ok(())
        } else {
            Err(bad(
                "the consent text id must be 1-64 characters of A-Z a-z 0-9 . _ -",
            ))
        }
    }
}

impl Settings {
    pub(crate) fn validate(&self) -> Result<(), Unavailable> {
        if !RETENTION_DAYS.contains(&self.retention_days) {
            return Err(bad(format!(
                "retention {} days is not offered",
                self.retention_days
            )));
        }
        if !BUDGET_MIB.contains(&self.budget_mib) {
            return Err(bad(format!(
                "budget {} MiB is not offered",
                self.budget_mib
            )));
        }
        match &self.consent {
            Some(consent) => consent.validate()?,
            None if self.enabled => return Err(bad("enabled without a consent record")),
            None => {}
        }
        Ok(())
    }
}

/// The settings as the bytes written to disk; refused if invalid or over
/// [`SETTINGS_MAX_BYTES`].
pub(crate) fn encode(settings: &Settings) -> Result<Vec<u8>, Unavailable> {
    settings.validate()?;
    let file = File {
        version: FORMAT_VERSION,
        enabled: settings.enabled,
        retention_days: settings.retention_days,
        budget_mib: settings.budget_mib,
        consent: settings.consent.clone(),
    };
    let bytes = serde_json::to_vec(&file).map_err(bad)?;
    if bytes.len() as u64 > SETTINGS_MAX_BYTES {
        return Err(bad("too large"));
    }
    Ok(bytes)
}

/// `None` when there is no settings file (never enabled).
pub(crate) fn load(location: &Location) -> Result<Option<Settings>, Unavailable> {
    let bytes = match crate::state_file::read(&location.settings_path(), SETTINGS_MAX_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(e) => return Err(bad(e)),
    };
    let file: File = serde_json::from_slice(&bytes).map_err(bad)?;
    if file.version != FORMAT_VERSION {
        return Err(bad(format!(
            "format version {} is not understood",
            file.version
        )));
    }
    let settings = Settings {
        enabled: file.enabled,
        retention_days: file.retention_days,
        budget_mib: file.budget_mib,
        consent: file.consent,
    };
    settings.validate()?;
    Ok(Some(settings))
}

/// Replace the settings file atomically (0600).
pub(crate) fn save(location: &Location, settings: &Settings) -> Result<(), Unavailable> {
    let bytes = encode(settings)?;
    crate::state_file::write(&location.settings_path(), &bytes).map_err(bad)
}
