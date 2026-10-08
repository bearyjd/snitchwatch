//! The user's curated-defaults choices, kept with the other bridge state:
//! which entries they turned on, the copy Snitchwatch installed of each
//! (to tell an edited rule from ours), and which were deleted outside
//! Snitchwatch (never reinstalled until turned on again).
//!
//! Saved as one small JSON file through [`crate::state_file`] (owner-only,
//! no links, no FIFOs, atomic replace). A file that fails any check is an
//! error: the caller keeps the choices in memory and leaves the file alone.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::Rule;

/// Everything the curated defaults remember.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Choices {
    /// Entries the user turned on. Empty by default: nothing is on unless
    /// the user says so.
    pub enabled: BTreeSet<String>,
    /// What Snitchwatch installed, by entry id.
    pub installed: BTreeMap<String, InstalledCopy>,
    /// Entries installed once and then deleted outside Snitchwatch.
    pub deleted_by_user: BTreeSet<String>,
}

impl Choices {
    /// The user turned `id` on. A deletion they made earlier no longer
    /// holds: they asked for it again.
    pub fn enable(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.enabled.insert(id.to_string());
        next.deleted_by_user.remove(id);
        next
    }

    pub fn disable(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.enabled.remove(id);
        next
    }

    /// Record a confirmed install of `rule` for `id`.
    pub fn installed(&self, id: &str, rule: &Rule) -> Self {
        let mut next = self.clone();
        next.installed
            .insert(id.to_string(), InstalledCopy::of(rule));
        next
    }

    /// Record a confirmed delete of `id`'s rule.
    pub fn removed(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.installed.remove(id);
        next
    }
}

/// The rule Snitchwatch sent for an entry, in the wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledCopy {
    pub name: String,
    pub rule: serde_json::Value,
}

impl InstalledCopy {
    pub fn of(rule: &Rule) -> Self {
        Self {
            name: rule.name.clone(),
            rule: crate::rule_wire::rule_to_wire(rule),
        }
    }

    /// Whether the daemon's `rule` is this copy, apart from `enabled`.
    pub fn matches(&self, rule: &Rule) -> bool {
        rule.name == self.name
            && crate::rule_wire::rule_from_wire(&self.rule)
                .is_ok_and(|ours| super::reconcile::same_ignoring_enabled(&ours, rule))
    }
}

/// The file in the bridge's state directory.
pub const FILE_NAME: &str = "curated-defaults.json";
/// Far more than the choices for a few hundred entries.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_IDS: usize = 256;
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileFormat {
    version: u32,
    choices: Choices,
}

/// The saved choices; `None` when there is no file.
pub fn load(path: &Path) -> io::Result<Option<Choices>> {
    let Some(bytes) = crate::state_file::read(path, MAX_FILE_BYTES)? else {
        return Ok(None);
    };
    let format: FileFormat = serde_json::from_slice(&bytes)
        .map_err(|_| crate::state_file::invalid("couldn't parse the file"))?;
    if format.version != VERSION {
        return Err(crate::state_file::invalid("unsupported version"));
    }
    validate(&format.choices)?;
    Ok(Some(format.choices))
}

/// Replace the file with `choices`, atomically.
pub fn save(path: &Path, choices: &Choices) -> io::Result<()> {
    validate(choices)?;
    let bytes = serde_json::to_vec(&FileFormat {
        version: VERSION,
        choices: choices.clone(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(crate::state_file::invalid(
            "the choices are too large to save",
        ));
    }
    crate::state_file::write(path, &bytes)
}

/// Every id is an entry id; every installed copy is a curated rule of the
/// name its id gives (what `save` writes, `load` accepts).
fn validate(choices: &Choices) -> io::Result<()> {
    let ids = choices
        .enabled
        .iter()
        .chain(&choices.deleted_by_user)
        .chain(choices.installed.keys());
    let count = choices.enabled.len() + choices.deleted_by_user.len() + choices.installed.len();
    if count > 3 * MAX_IDS || !ids.into_iter().all(|id| super::valid_id(id)) {
        return Err(crate::state_file::invalid(
            "the file lists an invalid entry id",
        ));
    }
    for (id, copy) in &choices.installed {
        let rule = crate::rule_wire::rule_from_wire(&copy.rule)
            .map_err(|_| crate::state_file::invalid("an installed copy isn't a rule"))?;
        let expected = format!("{}{id}", crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX);
        if copy.name != expected
            || rule.name != expected
            || super::check_curated_rule(&rule).is_err()
        {
            return Err(crate::state_file::invalid(
                "an installed copy isn't a curated default",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
