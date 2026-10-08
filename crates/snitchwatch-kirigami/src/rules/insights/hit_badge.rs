//! "Unused" and "No hits since" badges (P2.6 Part 2, owner decision N2).
//!
//! A rule is **Unused** only when everything below holds, because the claim is
//! "this rule had a fair chance to be hit for 14 days and wasn't":
//!
//! - the rule is enabled, logs (`nolog` rules never produce events), is
//!   permanent (`always` or `until restart`), and isn't managed elsewhere;
//! - its count is 0;
//! - the counts are **saved across restarts** (without that, "unused" would be
//!   a fact about this session only), and counting plus the rule's own age
//!   cover the whole window. The rule's age is its `created` time; a rule of
//!   unknown age is never called unused;
//! - **no gap** in the counting overlaps the window. The bridge reports only
//!   its latest gap, so a gap inside the window is `last_gap >= now - window`
//!   (a gap of unknown time counts as inside). Then the badge says hits may
//!   have been missed instead.
//!
//! Every other eligible zero-count rule gets only "No hits since <time>": the
//! time counting (or the rule) began, with a note when hits may be missing.
//!
//! One thing the badge cannot know: when the rule was last *enabled*. A rule
//! turned on yesterday shows as unused if it has been created for 14 days. The
//! wording stays literal ("no hits counted in the last 14 days").

use super::is_managed;
use crate::rules::hits::{RowHits, RuleHitsView};
use crate::rules::row_store::Rule;

/// N2: the "unused" window.
pub const UNUSED_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitBadge {
    /// No hits counted over the whole window, with nothing known to be missed.
    Unused,
    /// No hits counted over the window, but hits may have been missed in it.
    MissedSome,
    /// No hits counted since `since_unix_ms` (not yet long enough to say more).
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
    let observed_from =
        created_ms.map_or(counting.since_unix_ms, |c| c.max(counting.since_unix_ms));
    let covers_window = created_ms.is_some() && now_ms.saturating_sub(observed_from) >= window_ms;
    if !(counting.persistent && covers_window) {
        return Some(HitBadge::Since {
            since_unix_ms: observed_from,
            lossy: counting.lossy,
        });
    }
    let gap_in_window = (counting.lossy || counting.last_gap_unix_ms.is_some())
        && counting
            .last_gap_unix_ms
            .is_none_or(|gap| gap >= now_ms.saturating_sub(window_ms));
    Some(if gap_in_window {
        HitBadge::MissedSome
    } else {
        HitBadge::Unused
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
