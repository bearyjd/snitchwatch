//! The user's curated-defaults choices, kept with the other bridge state:
//! which entries they turned on, the copy Snitchwatch installed of each (in
//! [`CanonicalRule`] form, to tell its own copy from an edited one), and
//! which were removed outside the Recommended page (never reinstalled until
//! turned off and on again).
//!
//! Saved as one small JSON file through [`crate::state_file`] (owner-only,
//! no links, no FIFOs, atomic replace). Reading:
//! - no file is the first run: nothing on;
//! - a file that fails a file check, isn't JSON of a known version, or has
//!   unknown fields is an **error**, and the caller changes nothing in the
//!   firewall (code review H1);
//! - inside a readable file, an id that isn't an entry id, or a recorded
//!   copy that isn't exactly that entry's rule (or, for an entry no longer
//!   in the list, a curated rule), is dropped with a warning (lenient: a
//!   copy from an older list then reads as edited, so it is left alone).
//!
//! Version 1 (the wire JSON of each copy) is still read; version 2 is
//! written.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::Rule;

use super::canonical::{canonical, CanonicalRule};
use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use crate::state_file::invalid;

/// Everything the curated defaults remember.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Choices {
    /// Entries the user turned on. Empty by default: nothing is on unless
    /// the user says so.
    pub enabled: BTreeSet<String>,
    /// Entries the user turned off. An entry in neither set is undecided:
    /// a copy already in the firewall is left as it is (re-review M1).
    pub disabled: BTreeSet<String>,
    /// What Snitchwatch installed, by entry id.
    pub installed: BTreeMap<String, CanonicalRule>,
    /// The ids Snitchwatch installed. Kept when a recorded copy is dropped
    /// as invalid, so a rule removed outside is still known as one we
    /// installed and isn't reinstalled (re-review M2).
    pub installed_ids: BTreeSet<String>,
    /// Entries whose rule was removed outside the Recommended page, or by
    /// its Remove button.
    pub deleted_by_user: BTreeSet<String>,
}

impl Choices {
    /// The user turned `id` on. Only a change from off forgets a removal:
    /// "Turn all on" over an entry already on reinstalls nothing (M1).
    pub fn enable(&self, id: &str) -> Self {
        let mut next = self.clone();
        if next.enabled.insert(id.to_string()) {
            next.deleted_by_user.remove(id);
        }
        next.disabled.remove(id);
        next
    }

    pub fn disable(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.enabled.remove(id);
        next.disabled.insert(id.to_string());
        next
    }

    /// Record a confirmed install of `rule` for `id`.
    pub fn installed(&self, id: &str, rule: &Rule) -> Self {
        let mut next = self.clone();
        next.installed.insert(id.to_string(), canonical(rule));
        next.installed_ids.insert(id.to_string());
        next
    }

    /// Record a confirmed delete of `id`'s rule.
    pub fn removed(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.installed.remove(id);
        next.installed_ids.remove(id);
        next
    }

    /// Whether Snitchwatch recorded installing `id`'s rule.
    pub fn was_installed(&self, id: &str) -> bool {
        self.installed_ids.contains(id) || self.installed.contains_key(id)
    }

    /// Record that the user had `id`'s rule removed: never reinstalled until
    /// they turn it off and on again.
    pub fn user_removed(&self, id: &str) -> Self {
        let mut next = self.removed(id);
        next.deleted_by_user.insert(id.to_string());
        next
    }
}

/// The file in the bridge's state directory.
pub const FILE_NAME: &str = "curated-defaults.json";
/// Far more than the choices for a few hundred entries.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_IDS: usize = 256;
const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileFormat {
    version: u32,
    choices: Choices,
}

/// Version 1: each installed copy as the GUI wire JSON.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileFormatV1 {
    #[allow(dead_code)]
    version: u32,
    choices: ChoicesV1,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct ChoicesV1 {
    enabled: BTreeSet<String>,
    installed: BTreeMap<String, InstalledCopyV1>,
    deleted_by_user: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstalledCopyV1 {
    #[allow(dead_code)]
    name: String,
    rule: serde_json::Value,
}

#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

/// The saved choices; `None` when there is no file. The error carries the
/// detail for the log; GUIs get a fixed reason.
pub fn load(path: &Path) -> io::Result<Option<Choices>> {
    let Some(bytes) = crate::state_file::read(path, MAX_FILE_BYTES)? else {
        return Ok(None);
    };
    let parse_error = |e: serde_json::Error| invalid(format!("couldn't parse the file: {e}"));
    let Versioned { version } = serde_json::from_slice(&bytes).map_err(parse_error)?;
    let choices = match version {
        1 => from_v1(serde_json::from_slice::<FileFormatV1>(&bytes).map_err(parse_error)?),
        VERSION => {
            serde_json::from_slice::<FileFormat>(&bytes)
                .map_err(parse_error)?
                .choices
        }
        other => return Err(invalid(format!("unsupported version {other}"))),
    };
    let count = choices.enabled.len()
        + choices.disabled.len()
        + choices.deleted_by_user.len()
        + choices.installed.len()
        + choices.installed_ids.len();
    if count > 5 * MAX_IDS {
        return Err(invalid("the file lists too many entries"));
    }
    Ok(Some(lenient(choices)))
}

fn from_v1(file: FileFormatV1) -> Choices {
    let installed_ids_v1 = file.choices.installed.keys().cloned().collect();
    let installed = file
        .choices
        .installed
        .into_iter()
        .filter_map(|(id, copy)| {
            let rule = crate::rule_wire::rule_from_wire(&copy.rule).ok()?;
            Some((id, canonical(&rule)))
        })
        .collect();
    Choices {
        enabled: file.choices.enabled,
        installed_ids: installed_ids_v1,
        installed,
        deleted_by_user: file.choices.deleted_by_user,
        ..Choices::default()
    }
}

/// Drop what isn't valid, with a warning; keep the rest.
fn lenient(choices: Choices) -> Choices {
    let next = cleaned(choices.clone());
    if next != choices {
        tracing::warn!("curated defaults: ignored saved ids or copies that aren't valid");
    }
    next
}

fn cleaned(choices: Choices) -> Choices {
    let valid = |id: &String| super::valid_id(id);
    // An installed id outlives a dropped copy (re-review M2).
    let installed_ids = choices
        .installed_ids
        .iter()
        .chain(choices.installed.keys())
        .filter(|id| valid(id))
        .cloned()
        .collect();
    Choices {
        enabled: choices.enabled.into_iter().filter(valid).collect(),
        disabled: choices.disabled.into_iter().filter(valid).collect(),
        deleted_by_user: choices.deleted_by_user.into_iter().filter(valid).collect(),
        installed_ids,
        installed: choices
            .installed
            .into_iter()
            .filter(|(id, copy)| valid_copy(id, copy))
            .collect(),
    }
}

/// A recorded copy is the entry's exact rule (security review L2: a crafted
/// copy can't make a different port or host read as unedited); for an id
/// no longer in the list, a curated rule of that name (only ever used to
/// delete an identical rule under the prefix).
fn valid_copy(id: &str, copy: &CanonicalRule) -> bool {
    if !super::valid_id(id) || copy.name != format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{id}") {
        return false;
    }
    match super::entries().iter().find(|entry| entry.id == id) {
        Some(entry) => *copy == canonical(&entry.rule()),
        None => super::check_curated_rule(&copy.to_rule()).is_ok(),
    }
}

/// Replace the file with `choices`, atomically. Refuses what `load` would
/// drop: the bridge only writes what it made.
pub fn save(path: &Path, choices: &Choices) -> io::Result<()> {
    if cleaned(choices.clone()) != *choices {
        return Err(invalid("the choices hold an invalid id or copy"));
    }
    let bytes = serde_json::to_vec(&FileFormat {
        version: VERSION,
        choices: choices.clone(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("the choices are too large to save"));
    }
    crate::state_file::write(path, &bytes)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
