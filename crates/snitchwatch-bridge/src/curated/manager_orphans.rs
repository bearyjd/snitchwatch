//! "No longer recommended" (#113): rules under the curated prefix that no
//! entry offers and Snitchwatch recorded no copy of
//! ([`crate::curated::orphans`]). GUIs are told of them, and one is deleted
//! only at the user's confirmed request and only if it is still an orphan
//! when the command is sent: the state guard is held from that re-check
//! through queue admission, as for an install. The Rules page keeps refusing
//! every command under the prefix; this is the only way to remove one.
use super::*;
use crate::curated::orphans::{is_orphan, orphans};
use crate::curated::valid_id;
use crate::curated::wire::CuratedOrphanSummary;

/// The daemon refused an orphan's delete after dropping it from memory: its
/// file may remain and bring it back when the service restarts.
pub(super) const ORPHAN_FILE_LEFT_REASON: &str =
    "The firewall service stopped using this rule but \
     couldn't remove its saved file, so it may come back when the service restarts.";

/// Why an orphan's last removal failed, on the daemon stream it failed on:
/// shown until that stream ends.
#[derive(Debug, Clone, Copy)]
pub(super) struct OrphanFailure {
    pub(super) generation: u64,
    pub(super) problem: &'static str,
    /// Refused: out of the daemon's memory, its file may remain.
    pub(super) file_left: bool,
}

impl CuratedDefaults {
    /// Queue the removal of the orphan `id`, asked for by a GUI. Refused
    /// (nothing queued) on an inert bridge, and for anything the cached list
    /// doesn't show as an orphan now, so the queue holds listed orphans only.
    /// A "file may remain" note stays until the daemon reconnects.
    pub(super) fn ask_orphan_removal(&self, id: &str) {
        let mut state = lock(&self.inner.state);
        if state.inert.is_some() || !valid_id(id) || !self.still_orphan(&rule_name(id), &state) {
            warn!(orphan = %id, "removal asked for a rule that isn't an orphan; ignored");
            return;
        }
        if !state
            .orphan_failures
            .get(id)
            .is_some_and(|failure| failure.file_left)
        {
            state.orphan_failures.remove(id);
        }
        state.orphan_removals.insert(id.to_string());
        state.requests += 1;
        drop(state);
        self.inner.wake.notify_one();
    }

    /// The orphans as GUIs are told of them: none while inert (the choices
    /// may be unknown, so "no recorded copy" can't be told) or while the
    /// rule list is unknown. A refused delete is listed, not removable,
    /// until the daemon stream ends. Locks the cache after `state`.
    pub(super) fn orphan_summaries(&self, state: &State) -> Vec<CuratedOrphanSummary> {
        let generation = self.generation();
        if state.inert.is_some() {
            return Vec::new();
        }
        let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rules) = cache.rules() else {
            return Vec::new();
        };
        let left_out: BTreeSet<String> = cache.left_out().keys().cloned().collect();
        let failure = |id: &str| {
            state
                .orphan_failures
                .get(id)
                .filter(|failure| failure.generation == generation)
                .copied()
        };
        let mut listed: Vec<CuratedOrphanSummary> =
            orphans(entries(), rules, &left_out, &state.choices)
                .into_iter()
                .map(|(id, enabled)| CuratedOrphanSummary {
                    problem: failure(&id)
                        .filter(|failure| !failure.file_left)
                        .map(|failure| failure.problem.to_string()),
                    id,
                    enabled,
                    file_left: false,
                })
                .collect();
        for (id, failure) in &state.orphan_failures {
            let shown = listed.iter().any(|orphan| &orphan.id == id);
            if failure.file_left && failure.generation == generation && !shown {
                listed.push(CuratedOrphanSummary {
                    id: id.clone(),
                    enabled: false,
                    file_left: true,
                    problem: Some(failure.problem.to_string()),
                });
            }
        }
        listed.sort_by(|a, b| a.id.cmp(&b.id));
        listed
    }

    /// The orphan removals to send this pass. An inert bridge sends none and
    /// forgets them; a failure from an earlier daemon stream is forgotten.
    pub(super) fn take_orphan_removals(&self, generation: u64) -> BTreeSet<String> {
        let mut state = lock(&self.inner.state);
        if state.inert.is_some() {
            state.orphan_removals.clear();
            state.orphan_failures.clear();
            return BTreeSet::new();
        }
        state
            .orphan_failures
            .retain(|_, failure| failure.generation == generation);
        std::mem::take(&mut state.orphan_removals)
    }

    /// Send the queued orphan removals; a reconnect mid-pass leaves the rest
    /// queued for the follow-up pass, which re-checks them on the new list.
    pub(super) async fn remove_orphans(&self, ids: BTreeSet<String>, generation: u64) {
        let mut ids = ids.into_iter();
        while let Some(id) = ids.next() {
            if self.is_inert() {
                break;
            }
            if self.generation() != generation {
                let mut state = lock(&self.inner.state);
                state.orphan_removals.insert(id);
                state.orphan_removals.extend(ids);
                break;
            }
            self.remove_orphan(&id, generation).await;
            self.announce_changed();
        }
    }

    /// Whether `name` is still an orphan in the live list, for `state`'s
    /// choices.
    fn still_orphan(&self, name: &str, state: &State) -> bool {
        let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
        let left_out: BTreeSet<String> = cache.left_out().keys().cloned().collect();
        cache
            .rules()
            .is_some_and(|rules| is_orphan(name, entries(), rules, &left_out, &state.choices))
    }

    async fn remove_orphan(&self, id: &str, generation: u64) {
        let name = rule_name(id);
        // The state guard stays held from the re-check through queue
        // admission, so the choices it read can't change in between. The
        // rule list can (a HELLO): the guarded dispatch re-reads it under
        // the cache lock and queues only on the same daemon stream, while
        // the name is still listed.
        let sent = {
            let state = lock(&self.inner.state);
            if state.inert.is_some() || self.generation() != generation {
                return;
            }
            if !self.still_orphan(&name, &state) {
                warn!(orphan = %id, "a rule asked to be removed is no longer an orphan; not sent");
                return;
            }
            let Some(command) = CuratedCommand::delete(&name) else {
                return;
            };
            self.inner
                .commands
                .send_curated_guarded_delete(command, generation)
        };
        if sent.is_err() && self.generation() != generation {
            // A HELLO came in: the follow-up pass re-checks it on the new list.
            lock(&self.inner.state)
                .orphan_removals
                .insert(id.to_string());
            return;
        }
        let outcome = Self::wait_sent(sent, Refusal::Remove).await;
        let mut state = lock(&self.inner.state);
        match outcome {
            Ok(()) => {
                info!(orphan = %id, "removed a rule no longer recommended, at the user's request");
                state.orphan_failures.remove(id);
            }
            Err(problem) if problem.daemon_refused => {
                warn!(orphan = %id, "removed a rule no longer recommended; its file stays");
                let failure = OrphanFailure {
                    generation,
                    problem: ORPHAN_FILE_LEFT_REASON,
                    file_left: true,
                };
                state.orphan_failures.insert(id.to_string(), failure);
            }
            Err(problem) => {
                if !problem.sticky {
                    // Busy: asked again at the next change of the pass's inputs.
                    state.orphan_removals.insert(id.to_string());
                }
                let failure = OrphanFailure {
                    generation,
                    problem: problem.text,
                    file_left: false,
                };
                state.orphan_failures.insert(id.to_string(), failure);
            }
        }
    }
}

#[cfg(test)]
#[path = "manager_orphans_tests.rs"]
mod tests;
