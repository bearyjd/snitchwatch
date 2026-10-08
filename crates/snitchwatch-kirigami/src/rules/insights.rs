//! Rule insights for the Rules tab (P2.6 Part 2; plan
//! `docs/superpowers/plans/2026-10-08-rule-insights.md`). Pure and Qt-free.
//!
//! - [`hit_badge`]: which rules had no hits, and how sure that is.
//! - [`shadow`]: rules that can never decide a connection because another
//!   one covers them (built on [`atoms`]).
//! - [`state`]: the on-demand analysis' lifecycle, so a result for an older
//!   rule list is never shown.
//! - [`row`]: what one row shows of all that.
//!
//! Insights only describe. Nothing here, and nothing the Rules tab shows
//! about them, says that a rule was or will be removed or changed.

pub mod atoms;
pub mod hit_badge;
pub mod row;
pub mod shadow;
pub mod state;

use super::row_store::Rule;

/// The packaged rules the bridge installs for itself.
const PACKAGED_RULE_PREFIX: &str = "000-snitchwatch-";

/// Rules managed elsewhere (blocklists, the packaged `000-snitchwatch-`
/// rules, anything the bridge marks read-only): they get no badge or finding,
/// because the user can't act on them here. They may still shadow a user rule.
pub fn is_managed(rule: &Rule) -> bool {
    rule.is_read_only()
        || rule.is_blocklist_sourced()
        || rule.name.starts_with(PACKAGED_RULE_PREFIX)
}

#[cfg(test)]
#[path = "insights/atoms_tests.rs"]
mod atoms_tests;
#[cfg(test)]
#[path = "insights/hit_badge_tests.rs"]
mod hit_badge_tests;
#[cfg(test)]
#[path = "insights/row_tests.rs"]
mod row_tests;
#[cfg(test)]
#[path = "insights/shadow_tests.rs"]
mod shadow_tests;
#[cfg(test)]
#[path = "insights/state_tests.rs"]
mod state_tests;
#[cfg(test)]
mod testkit;
