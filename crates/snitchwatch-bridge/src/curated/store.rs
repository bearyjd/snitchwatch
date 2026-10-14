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
//! Versions 1 (wire copies) and 2 are still read without broad authority;
//! version 3 additionally records reviewed policy consent, never tickets.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    /// Deliberate modern consent. An enabled legacy id alone grants nothing.
    #[serde(deserialize_with = "read_broad_grants")]
    pub broad_grants: BTreeMap<String, BroadGrant>,
    /// Entries whose rule file a refused delete may have left behind, which
    /// may load again at the daemon's next start (#120 item 18): the rules
    /// cache's marker doesn't outlive the bridge. Written only while set, so
    /// a file without one is still read by an older bridge.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub files_left: BTreeSet<String>,
}

pub const BROAD_CONSENT: &str = "resolver-any-address-v1";
const GRANT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BroadAuthority {
    Enable,
    AdoptionOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BroadGrant {
    pub version: u32,
    pub id: String,
    pub consent: String,
    pub policy_sha256: String,
    pub authority: BroadAuthority,
}

/// Ordered struct fields and ordered operator leaves define a stable JSON
/// encoding; canonical excludes only transient enabled/created values.
pub fn broad_policy_fingerprint(entry: &super::CuratedEntry) -> String {
    let bytes = serde_json::to_vec(&canonical(&entry.rule()))
        .expect("canonical policy contains only JSON-serializable fields");
    format!("{:x}", Sha256::digest(bytes))
}

impl BroadGrant {
    fn matches(&self, entry: &super::CuratedEntry) -> bool {
        entry.any_address
            && self.version == GRANT_VERSION
            && self.id == entry.id
            && self.consent == BROAD_CONSENT
            && self.policy_sha256 == broad_policy_fingerprint(entry)
    }
}

/// Malformed provenance drops authority, not otherwise readable choices.
/// Strict grant decoding rejects unknown/duplicate fields and wrong types.
fn read_broad_grants<'de, D>(deserializer: D) -> Result<BTreeMap<String, BroadGrant>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Grant {
        Valid(BroadGrant),
        Invalid(serde::de::IgnoredAny),
    }
    struct UniqueGrants(BTreeMap<String, Grant>);
    impl<'de> Deserialize<'de> for UniqueGrants {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = UniqueGrants;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a map with unique broad grant ids")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut grants = BTreeMap::new();
                    while let Some((id, grant)) = map.next_entry::<String, Grant>()? {
                        if grants.insert(id, grant).is_some() {
                            return Err(serde::de::Error::custom("duplicate broad grant id"));
                        }
                    }
                    Ok(UniqueGrants(grants))
                }
            }
            deserializer.deserialize_map(Visitor)
        }
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Grants {
        Map(UniqueGrants),
        Invalid(serde::de::IgnoredAny),
    }
    Ok(match Grants::deserialize(deserializer)? {
        Grants::Map(UniqueGrants(grants)) => grants
            .into_iter()
            .filter_map(|(id, grant)| match grant {
                Grant::Valid(grant) => Some((id, grant)),
                Grant::Invalid(_) => None,
            })
            .collect(),
        Grants::Invalid(_) => BTreeMap::new(),
    })
}

impl Choices {
    /// Revocation alone preserves the rule's user's enabled/disabled choice:
    /// a Rules-page toggle off leaves its curated adoption record intact.
    pub fn revoke_broad(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.broad_grants.remove(id);
        next
    }
    /// Admission must already have validated a live modern intent. This
    /// records its reviewed scope and deliberately does not change choices.
    pub fn grant_broad(&self, id: &str, activation: bool) -> Self {
        let mut next = self.clone();
        if let Some(entry) = super::entries()
            .iter()
            .find(|entry| entry.id == id && entry.any_address)
        {
            next.broad_grants.insert(
                id.into(),
                BroadGrant {
                    version: GRANT_VERSION,
                    id: id.into(),
                    consent: BROAD_CONSENT.into(),
                    policy_sha256: broad_policy_fingerprint(entry),
                    authority: if activation {
                        BroadAuthority::Enable
                    } else {
                        BroadAuthority::AdoptionOnly
                    },
                },
            );
        }
        next
    }

    pub fn has_broad_authority(&self, id: &str) -> bool {
        super::entries()
            .iter()
            .find(|entry| entry.id == id)
            .is_some_and(|entry| self.has_broad_authority_for(entry))
    }

    pub fn has_broad_authority_for(&self, entry: &super::CuratedEntry) -> bool {
        self.enabled.contains(&entry.id)
            && !self.disabled.contains(&entry.id)
            && !self.deleted_by_user.contains(&entry.id)
            && self.broad_grants.get(&entry.id).is_some_and(|grant| {
                grant.authority == BroadAuthority::Enable && grant.matches(entry)
            })
    }

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
        next.broad_grants.remove(id);
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

    /// Record that `id`'s rule file may remain (a refused delete).
    pub fn file_left(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.files_left.insert(id.to_string());
        next
    }

    /// Forget that: the file was rewritten or removed, or there was none.
    pub fn file_gone(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.files_left.remove(id);
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
        next.broad_grants.remove(id);
        next
    }
}

/// The file in the bridge's state directory.
pub const FILE_NAME: &str = "curated-defaults.json";
/// Far more than the choices for a few hundred entries.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_IDS: usize = 256;
const VERSION: u32 = 3;

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
        2 | VERSION => {
            let mut choices = serde_json::from_slice::<FileFormat>(&bytes)
                .map_err(parse_error)?
                .choices;
            if version == 2 {
                choices.broad_grants.clear();
            }
            choices
        }
        other => return Err(invalid(format!("unsupported version {other}"))),
    };
    let count = choices.enabled.len()
        + choices.disabled.len()
        + choices.deleted_by_user.len()
        + choices.installed.len()
        + choices.installed_ids.len()
        + choices.broad_grants.len()
        + choices.files_left.len();
    if count > 7 * MAX_IDS {
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
        files_left: choices.files_left.into_iter().filter(valid).collect(),
        installed_ids,
        broad_grants: choices
            .broad_grants
            .into_iter()
            .filter(|(id, grant)| {
                super::entries()
                    .iter()
                    .find(|entry| entry.id == *id)
                    .is_some_and(|entry| grant.matches(entry))
            })
            .collect(),
        installed: choices
            .installed
            .into_iter()
            .filter(|(id, copy)| valid_copy(id, copy))
            .collect(),
    }
}

/// A recorded copy is the entry's exact rule (security review L2: a crafted
/// copy can't make a different port or host read as unedited), a run-time
/// account's at the uid it was installed with; for an id
/// no longer in the list, a curated rule of that name (only ever used to
/// delete an identical rule under the prefix).
fn valid_copy(id: &str, copy: &CanonicalRule) -> bool {
    if !super::valid_id(id) || copy.name != format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{id}") {
        return false;
    }
    match super::entries().iter().find(|entry| entry.id == id) {
        Some(entry) => entry
            .rule_matching(&copy.to_rule())
            .is_some_and(|ours| *copy == canonical(&ours)),
        None => super::check_curated_rule(&copy.to_rule()).is_ok(),
    }
}

/// Replace the file with `choices`, atomically. Refuses what `load` would
/// drop: the bridge only writes what it made.
pub fn save(path: &Path, choices: &Choices) -> io::Result<()> {
    // Retain owner-only/no-link checks at the durable authority boundary.
    // Atomic replacement must not silently repair a planted linked store.
    if path.exists() {
        crate::state_file::read(path, MAX_FILE_BYTES)?;
    }

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
