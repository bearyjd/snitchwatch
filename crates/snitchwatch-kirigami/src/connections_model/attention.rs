//! `ConnectionsModel`'s exception-first default (clarity 0c; plan
//! `docs/superpowers/plans/2026-10-27-exception-first-default.md`): the
//! `needs_attention` switch, the first-seen baseline and acknowledgement,
//! and the "Background and system" counts. Which rows are handled is
//! `connections::attention`; this only keeps its facts current and forces
//! the projection to follow them.

use std::collections::HashSet;
use std::sync::Arc;

use super::*;
use crate::connections::attention::AttentionFacts;
use crate::connections::seen_programs::{SeenPrograms, SEEN_FILE_NAME};

impl qobject::ConnectionsModel {
    pub(super) fn set_needs_attention(self: Pin<&mut Self>, on: bool) {
        let mut filter = self.store.filter().clone();
        filter.set_needs_attention(on);
        self.apply_filter(filter);
    }

    pub(super) fn mark_new_programs_seen(mut self: Pin<&mut Self>) {
        if self.as_mut().rust_mut().seen.acknowledge(now_ms()) {
            self.refresh_attention_facts();
        }
    }

    pub(super) fn dismiss_baseline_notice(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().seen.dismiss_notice();
        self.refresh_attention_counts();
    }

    /// The live GUI keeps its seen set in the config directory; headless
    /// tests and demo mode never touch it (`SeenPrograms::default`).
    pub(super) fn use_persisted_seen_programs(mut self: Pin<&mut Self>) {
        if crate::demo::enabled() {
            return;
        }
        let path = crate::paths::config_dir().join(SEEN_FILE_NAME);
        self.as_mut().rust_mut().seen =
            SeenPrograms::persisted(path, crate::logging::effective_uid());
    }

    /// A `SetRules`/`UpdateRules` was applied: take the baseline if this is
    /// the first one after a clear, then re-decide every row.
    pub(super) fn after_rules_applied(mut self: Pin<&mut Self>) {
        {
            let mut rust = self.as_mut().rust_mut();
            let ConnectionsModelRust { store, seen, .. } = &mut *rust;
            let present = store
                .rows()
                .iter()
                .filter_map(|row| row.process_path.as_deref());
            seen.on_rules_applied(present, now_ms());
        }
        self.refresh_attention_facts();
    }

    /// Rebuild the "handled" facts from the CURRENT rules list and seen set;
    /// when they changed, recompute the visible rows, so a rule deleted or
    /// disabled later shows its rows again.
    pub(super) fn refresh_attention_facts(mut self: Pin<&mut Self>) {
        let facts = AttentionFacts::new(self.level_rules.rules(), self.seen.seen_paths());
        if self.store.filter().attention_facts() == Some(&facts) {
            self.refresh_attention_counts();
            return;
        }
        let facts = Arc::new(facts);
        if self.grouped {
            self.as_mut().rust_mut().store.set_attention_facts(facts);
            let projection = self.compute_grouped_projection();
            self.as_mut().apply_grouped_projection(projection);
        } else {
            unsafe {
                self.as_mut().begin_reset_model();
            }
            self.as_mut().rust_mut().store.set_attention_facts(facts);
            unsafe {
                self.as_mut().end_reset_model();
            }
        }
        self.as_mut().refresh_filter_state();
        self.refresh_counts();
    }

    /// The group's one-line summary figures, the new-program count and the
    /// baseline notice.
    pub(super) fn refresh_attention_counts(mut self: Pin<&mut Self>) {
        let (count, programs) = {
            let filter = self.store.filter();
            let mut count = 0;
            let mut programs = HashSet::new();
            for row in self
                .store
                .rows()
                .iter()
                .filter(|row| filter.is_handled(row))
            {
                count += 1;
                programs.insert(row.process_path.as_deref());
            }
            (count, programs.len() as i32)
        };
        let new_programs = self.seen.new_count() as i32;
        let notice = QString::from(self.seen.notice());
        self.as_mut().set_background_count(count);
        self.as_mut().set_background_programs(programs);
        self.as_mut().set_new_program_count(new_programs);
        self.as_mut().set_baseline_notice(notice);
    }
}
