//! "Unused" and "No hits since" badges (P2.6 Part 2, owner decision N2).
//!
//! A rule is **Unused** only when everything below holds, because the claim is
//! "this rule had a fair chance to be hit for 14 days and wasn't":
//!
//! - the rule is enabled, logs (`nolog` rules never produce events), is
//!   permanent (`always` or `until restart`), has a name the bridge can count,
//!   and isn't managed elsewhere;
//! - its count is 0;
//! - the counts are **saved across restarts** (without that, "unused" would be
//!   a fact about this session only);
//! - the **period it was fairly counted in** is at least 14 days. That period
//!   starts at the latest of: when counting began, when the rule was created
//!   (its `created` time; a rule of unknown age is never called unused), and
//!   the bridge's **last gap** (a restart, a daemon restart, a burst past the
//!   daemon's event cap: moments hits may have been lost before). A gap of
//!   unknown time leaves no period to trust.
//!
//! Every other eligible zero-count rule gets only "No hits since <time>": the
//! start of that same period, which is the last gap when that is the latest of
//! the three. (A gap of unknown time keeps the counting start and adds that
//! hits may have been missed.)
//!
//! Because the period starts after the last gap, one bridge restart delays
//! "unused" by 14 days instead of ruling it out for good, and an old gap
//! costs nothing.
//!
//! What the badge cannot know: when the rule was last *enabled*, or put back
//! by something other than Snitchwatch. An edit or re-enable made here
//! restamps its `created` (the daemon rebuilds the rule and the bridge follows,
//! `RulesCache::upsert`), which starts a new period. One made by editing the
//! rule file does not, and a rule that was away and returns with its old
//! `created` leaves a gap only if it had counted hits (the bridge's
//! `adopt_snapshot`): one with none that returns after 14 days reads
//! "Unused" at once. The wording stays literal ("no hits counted in the last
//! 14 days").

use super::is_managed;
use crate::rules::hits::{RowHits, RuleHitsView};
use crate::rules::row_store::Rule;

/// N2: the "unused" window.
pub const UNUSED_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitBadge {
    /// No hits counted over a trusted period of at least the window.
    Unused,
    /// No hits counted since `since_unix_ms`, the start of the trusted
    /// period (not yet long enough to say more). `lossy` only for a gap of
    /// unknown time: hits may have been missed, and the period is unknown.
    Since { since_unix_ms: i64, lossy: bool },
}

/// The badge for `rule`, if it deserves one. `None` for a rule that was hit,
/// can't be counted, is managed elsewhere, or when the bridge sent no counts.
pub fn hit_badge(
    rule: &Rule,
    hits: &RuleHitsView,
    now_ms: i64,
    window_ms: i64,
) -> Option<HitBadge> {
    if is_managed(rule)
        || !rule.enabled
        || rule.nolog
        || !matches!(rule.duration.as_str(), "always" | "until restart")
    {
        return None;
    }
    let counting = hits.counting()?;
    if !matches!(hits.for_rule(rule), RowHits::Counted { count: 0, .. }) {
        return None;
    }
    let created_ms = (rule.created > 0).then(|| rule.created.saturating_mul(1000));
    let unknown_gap = counting.lossy && counting.last_gap_unix_ms.is_none();
    // The trusted period starts at the latest of the three.
    let from = [
        Some(counting.since_unix_ms),
        created_ms,
        counting.last_gap_unix_ms,
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(counting.since_unix_ms);
    let long_enough =
        created_ms.is_some() && !unknown_gap && now_ms.saturating_sub(from) >= window_ms;
    Some(if counting.persistent && long_enough {
        HitBadge::Unused
    } else {
        HitBadge::Since {
            since_unix_ms: from,
            lossy: unknown_gap,
        }
    })
}

/// The names of the rules that are [`HitBadge::Unused`].
pub fn unused(rules: &[Rule], hits: &RuleHitsView, now_ms: i64, window_ms: i64) -> Vec<String> {
    rules
        .iter()
        .filter(|rule| hit_badge(rule, hits, now_ms, window_ms) == Some(HitBadge::Unused))
        .map(|rule| rule.name.clone())
        .collect()
}
