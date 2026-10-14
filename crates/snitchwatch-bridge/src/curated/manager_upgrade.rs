//! Replacing a copy installed before its entry's sender pin (#120 36/41),
//! or one at a uid its account no longer has (`manager_moved.rs`).
//!
//! Opensnitchd's `replaceUserRule` swaps a rule of the same name in memory,
//! keeping the old one if the new one doesn't compile, so one `CHANGE_RULE`
//! moves an opted-in entry to its pinned rule with no moment where it is
//! missing. It asks nothing: it only narrows what the user chose, or keeps
//! it on the same account. Sent only while the entry is on and its account
//! confirmed (`withhold`). A failure isn't sent again on the same daemon
//! stream.
//! - A copy from before the pin stays on as it is when refused: that copy
//!   is the status quo the user chose.
//! - A moved copy refused takes #130's path, as a refused install: the
//!   entry is turned off, its grant revoked, and the next pass deletes the
//!   name, as the old uid may now be another account's.

use super::resolver::account_uid;
use super::state::{fail, keep, track_maybe_applied, Refusal};
use super::*;

impl CuratedDefaults {
    pub(super) async fn upgrade(&self, id: &str, generation: u64, revision: u64) {
        let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
            return;
        };
        let name = entry.rule_name();
        let (sent, rule, status, moved) = {
            let state = lock(&self.inner.state);
            if state.inert.is_some()
                || self.generation() != generation
                || !state.choices.enabled.contains(id)
                || state.choices.deleted_by_user.contains(id)
                || state.withheld.contains(id)
            {
                return;
            }
            let uid = account_uid(entry, &state.found);
            let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
            let Some(current) = cache
                .rules()
                .and_then(|rules| rules.get(&entry.rule_name()))
            else {
                return;
            };
            // Turning the broad rule on needs its live grant, as an install
            // does (#128).
            let guarded = entry.broad() && current.enabled;
            if guarded
                && (state.broad_revision != revision
                    || !state.choices.has_broad_authority(id)
                    || state.saved < state.version
                    || !state.storage.persistent)
            {
                return;
            }
            let command = if guarded {
                CuratedCommand::authorized_upgrade(entry, current, uid)
            } else {
                CuratedCommand::upgrade(entry, current, uid)
            };
            let Some(command) = command else {
                return;
            };
            let moved = !is_legacy(entry, current);
            let status = if current.enabled {
                EntryStatus::Installed
            } else {
                EntryStatus::InstalledButOff
            };
            drop(cache);
            let rule = command.rule().cloned();
            let sent = if guarded {
                self.inner
                    .commands
                    .send_curated_guarded_replace(command, generation)
            } else {
                self.inner.commands.send_curated(command)
            };
            (sent, rule, status, moved)
        };
        let outcome = Self::wait_sent(sent, Refusal::Add).await;
        let marked = self.file_left(&name);
        let mut state = lock(&self.inner.state);
        if moved {
            // As an install: a refused or unanswered rewrite may apply
            // unlisted (#130, #120 item 13).
            let marked = marked || state.choices.files_left.contains(id);
            track_maybe_applied(&mut state, &name, true, &outcome, marked, generation);
        }
        match (outcome, rule) {
            (Ok(()), Some(rule)) => {
                info!(entry = %id, "pinned a recommended rule to its sender");
                let choices = state.choices.installed(id, &rule);
                keep(&mut state, choices);
                state.failures.remove(id);
                state.statuses.insert(id.to_string(), status);
                if moved {
                    state.repinned.insert(id.to_string(), generation);
                }
            }
            (Ok(()), None) => {}
            (Err(problem), _) if moved && problem.daemon_refused => {
                warn!(entry = %id, "the firewall service refused a re-pinned rule; turned off");
                self.revoke_locked(&mut state, id);
                let choices = state.choices.disable(id);
                keep(&mut state, choices);
                state.failures.remove(id);
                state
                    .statuses
                    .insert(id.to_string(), EntryStatus::NotInstalled);
                state
                    .install_refusals
                    .insert(id.to_string(), (INSTALL_REFUSED_REASON, generation));
            }
            (Err(problem), _) => {
                warn!(entry = %id, problem = problem.text, "a recommended rule wasn't pinned to its sender");
                fail(&mut state, false, id, status, problem, generation);
            }
        }
    }
}
