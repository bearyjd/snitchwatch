//! Replacing a copy installed before its entry's sender pin (#120 36/41),
//! or one at a uid its account no longer has (`manager_moved.rs`, switch
//! A).
//!
//! Opensnitchd's `replaceUserRule` swaps a rule of the same name in memory,
//! keeping the old one if the new one doesn't compile, so one `CHANGE_RULE`
//! moves an opted-in entry to its pinned rule with no moment where it is
//! missing. It only narrows what the user chose, so it asks nothing. Sent
//! only while the entry is on and its account confirmed (`withhold`). A
//! failure leaves the entry on with its old copy, and isn't sent again on
//! the same daemon stream: it is not #130's refused install, since the old
//! rule never left the daemon's memory and the user's choice is unchanged.

use super::resolver::account_uid;
use super::state::{fail, keep, Refusal};
use super::*;

impl CuratedDefaults {
    pub(super) async fn upgrade(&self, id: &str, generation: u64, revision: u64) {
        let Some(entry) = entries().iter().find(|entry| entry.id == id) else {
            return;
        };
        let (sent, rule, status) = {
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
            (sent, rule, status)
        };
        let outcome = Self::wait_sent(sent, Refusal::Add).await;
        let mut state = lock(&self.inner.state);
        match (outcome, rule) {
            (Ok(()), Some(rule)) => {
                info!(entry = %id, "pinned a recommended rule to its sender");
                let choices = state.choices.installed(id, &rule);
                keep(&mut state, choices);
                state.failures.remove(id);
                state.statuses.insert(id.to_string(), status);
            }
            (Ok(()), None) => {}
            (Err(problem), _) => {
                warn!(entry = %id, problem = problem.text, "a recommended rule wasn't pinned to its sender");
                fail(&mut state, false, id, status, problem, generation);
            }
        }
    }
}
