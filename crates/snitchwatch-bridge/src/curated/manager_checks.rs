//! The live re-checks a pass makes just before it sends a planned command
//! (code review LOW-7): whether the user still wants it and the daemon's
//! list still calls for it. Split from `manager.rs`, which calls them.
use super::*;

impl CuratedDefaults {
    /// Whether the user still wants `action` (a choice may change while
    /// earlier commands of the pass wait for the daemon), and, for an
    /// install, whether the daemon still lacks the rule; for a delete,
    /// whether its copy is still unedited, or, after a refused install,
    /// still unlisted.
    pub(super) fn still_wanted(&self, action: &CuratedAction) -> bool {
        let state = lock(&self.inner.state);
        if state.inert.is_some() {
            return false;
        }
        match action {
            CuratedAction::Install(id) => {
                let wanted = state.choices.enabled.contains(id)
                    && !state.choices.deleted_by_user.contains(id);
                drop(state);
                wanted && self.still_missing(&format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{id}"))
            }
            CuratedAction::Upgrade(id) => state.choices.enabled.contains(id),
            CuratedAction::Delete { id, name } => {
                let wanted =
                    !state.choices.enabled.contains(id) || !entries().iter().any(|e| &e.id == id);
                let recorded = state.choices.installed.get(id).cloned();
                let maybe_applied = state.maybe_applied.contains_key(name);
                drop(state);
                wanted
                    && (self.still_unedited(id, name, recorded.as_ref())
                        || (maybe_applied && self.still_missing(name)))
            }
        }
    }

    /// The live list's copy under `name` is still unedited (or the entry's
    /// copy from before its sender pin): a copy edited since the pass began
    /// is left alone.
    pub(super) fn still_unedited(
        &self,
        id: &str,
        name: &str,
        recorded: Option<&super::super::canonical::CanonicalRule>,
    ) -> bool {
        let entry = entries().iter().find(|entry| entry.id == id);
        let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .rules()
            .and_then(|rules| rules.get(name))
            .is_some_and(|rule| {
                is_unedited(entry, recorded, rule) || entry.is_some_and(|e| is_legacy(e, rule))
            })
    }

    /// The live list still lacks `name` (code review LOW-7): a copy that
    /// arrived since the pass began is adopted next pass, not overwritten.
    pub(super) fn still_missing(&self, name: &str) -> bool {
        let cache = self.inner.rules.lock().unwrap_or_else(|e| e.into_inner());
        cache.rules().is_some_and(|rules| !rules.contains_key(name))
            && !cache.left_out().contains_key(name)
    }
}
