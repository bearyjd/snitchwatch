//! What a rule command the daemon answered `ERROR` left in opensnitchd v1.8.0
//! (tower r12; `docs/superpowers/plans/2026-10-08-refused-delete-honesty.md`),
//! and the "file may remain" marker it can leave.
//!
//! Decided from the command's shape, never from the daemon's text, which a
//! fork or a forged TCP stream can word as it likes (`rule/loader.go`):
//! - `DELETE_RULE`: `Delete` drops the rule from memory, then removes an
//!   `always` rule's file, and only that can fail. So the rule no longer
//!   applies, and its file may remain and load again at the next start. A
//!   temporary rule has no file and never fails.
//! - `CHANGE_RULE` of a disabled `always` rule: a disabled rule isn't
//!   compiled, so only `Save` can fail, after the rule as sent replaced the
//!   old one in memory. Its file is the old one, or none.
//! - Any other `CHANGE_RULE`: a compile error leaves the old rule, a `Save`
//!   failure the new one; the bridge can't tell which, so the cache stays as
//!   it was. A disabled temporary rule can't fail at all.

use std::collections::BTreeSet;

use snitchwatch_proto::protocol::{Action, Notification, Rule};
use tracing::warn;

use super::{now_secs, RulesCache, RulesSync};

/// Most names [`RulesCache::files_left`] holds; later ones aren't recorded.
pub const MAX_FILES_LEFT: usize = 256;

/// What a refused command left in the daemon's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusedEffect {
    /// Its rules no longer apply; their files may remain.
    Removed,
    /// Its rule applies as sent; its file wasn't written.
    AppliedNotSaved,
    /// Nothing the bridge can place: the cache stays as it was.
    Unknown,
}

/// Stock only: an `ERROR` reply to `DELETE_RULE` means the rule already
/// left the daemon's memory. That is true for stock opensnitchd v1.8.0
/// `Loader.Delete` (memory first, then the file); a daemon that refuses
/// before touching memory (e.g. the proposed tower PR #93) would break
/// this, and the cache would drop a rule that still applies.
fn refused_effect(sent: &Notification) -> RefusedEffect {
    if sent.r#type == Action::DeleteRule as i32 {
        return RefusedEffect::Removed;
    }
    // `Deserialize` refuses only a rule without an operator, before
    // anything changes; the bridge never sends one.
    let disabled_always =
        |rule: &Rule| !rule.enabled && rule.duration == "always" && rule.operator.is_some();
    if sent.r#type == Action::ChangeRule as i32
        && !sent.rules.is_empty()
        && sent.rules.iter().all(disabled_always)
    {
        return RefusedEffect::AppliedNotSaved;
    }
    RefusedEffect::Unknown
}

impl RulesCache {
    /// Deleted rules the daemon stopped using but couldn't remove the file
    /// of. A name leaves when a snapshot lists it again (the file loaded) or
    /// a confirmed `always` change rewrites its file; not on a confirmed
    /// delete, which for a name not in memory touches no file. The curated
    /// defaults also drop a rule that never had a file
    /// ([`Self::forget_file_left`]).
    pub fn files_left(&self) -> &BTreeSet<String> {
        &self.files_left
    }

    /// Forget `name`'s marker; bumps the revision if it had one. Also for
    /// a caller that knows the rule had no file: the curated defaults, for
    /// a rule whose install was refused (#120 item 15).
    pub(crate) fn forget_file_left(&mut self, name: &str) {
        if self.files_left.remove(name) {
            self.revision += 1;
        }
    }

    fn mark_file_left(&mut self, name: &str) {
        if self.files_left.contains(name) {
            return;
        }
        if self.files_left.len() >= MAX_FILES_LEFT {
            warn!("too many rule files the daemon couldn't remove; not noting another");
            return;
        }
        self.files_left.insert(name.to_string());
        self.revision += 1;
    }

    /// Apply a command the daemon answered `ERROR`, now.
    pub fn apply_refused(&mut self, sent: &Notification) -> bool {
        self.apply_refused_at(sent, now_secs())
    }

    /// [`apply_refused`](Self::apply_refused) at a given time. Returns
    /// whether the cache changed.
    pub fn apply_refused_at(&mut self, sent: &Notification, now_secs: i64) -> bool {
        let before = self.revision;
        match refused_effect(sent) {
            RefusedEffect::Removed => {
                for rule in &sent.rules {
                    self.remove(&rule.name);
                    self.mark_file_left(&rule.name);
                }
            }
            RefusedEffect::AppliedNotSaved => {
                for rule in &sent.rules {
                    let restamped = Rule {
                        created: now_secs,
                        ..rule.clone()
                    };
                    self.upsert_at(restamped, now_secs);
                }
            }
            RefusedEffect::Unknown => {}
        }
        self.revision != before
    }
}

impl RulesSync {
    /// A command the daemon answered `ERROR`, in reply order. Published like
    /// [`apply_confirmed`](Self::apply_confirmed), but only if it changed
    /// the cache.
    pub(crate) fn apply_refused(&self, sent: &Notification) {
        let changed = {
            let mut cache = super::lock(&self.cache);
            let changed = cache.apply_refused(sent);
            if refused_effect(sent) == RefusedEffect::Removed {
                self.hits
                    .forget(sent.rules.iter().map(|rule| rule.name.as_str()));
            }
            changed
        };
        if changed {
            self.publish_after_command();
        }
    }
}
