use super::hit_badge::{hit_badge, unused, HitBadge, UNUSED_WINDOW_MS};
use super::testkit::*;
use crate::rules::hits::RuleHitsView;
use crate::rules::row_store::Rule;
use snitchwatch_bridge::ws_messages::{RuleHitWire, ServerMessage, StorageStatus};

const DAY: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_800_000_000_000;
const W: i64 = UNUSED_WINDOW_MS;

struct Counts {
    since: Option<i64>,
    persistent: bool,
    lossy: bool,
    last_gap: Option<i64>,
    hits: Vec<(&'static str, u64)>,
}

impl Counts {
    /// Counting for 30 days, saved, complete, with `a` hit twice.
    fn healthy() -> Self {
        Self {
            since: Some(NOW - 30 * DAY),
            persistent: true,
            lossy: false,
            last_gap: None,
            hits: vec![("a", 2)],
        }
    }

    fn view(&self) -> RuleHitsView {
        let mut view = RuleHitsView::default();
        view.apply(&ServerMessage::RuleHits {
            since_unix_ms: self.since,
            lossy: self.lossy,
            last_gap_unix_ms: self.last_gap,
            storage: StorageStatus {
                persistent: self.persistent,
                reason: None,
                unreadable: false,
            },
            hits: self
                .hits
                .iter()
                .map(|(name, count)| RuleHitWire {
                    name: (*name).to_string(),
                    count: *count,
                    last_hit_unix_ms: NOW - DAY,
                })
                .collect(),
        });
        view
    }
}

/// An enabled, permanent rule created `age_days` ago.
fn aged(name: &str, age_days: i64) -> Rule {
    let mut r = allow(name, host("example.com"));
    r.created = (NOW - age_days * DAY) / 1000;
    r
}

fn badge(rule: &Rule, counts: &Counts) -> Option<HitBadge> {
    hit_badge(rule, &counts.view(), NOW, W)
}

#[test]
fn zero_hits_over_the_whole_window_is_unused() {
    assert_eq!(
        badge(&aged("b", 20), &Counts::healthy()),
        Some(HitBadge::Unused)
    );
    let names = unused(
        &[aged("b", 20), aged("a", 20)],
        &Counts::healthy().view(),
        NOW,
        W,
    );
    assert_eq!(names, vec!["b".to_string()], "a was hit; only b is unused");
}

#[test]
fn a_rule_that_was_hit_is_not_unused() {
    assert_eq!(badge(&aged("a", 20), &Counts::healthy()), None);
}

#[test]
fn a_nolog_rule_is_excluded() {
    let mut rule = aged("b", 20);
    rule.nolog = true;
    assert_eq!(badge(&rule, &Counts::healthy()), None);
}

#[test]
fn a_disabled_rule_is_excluded() {
    let mut rule = aged("b", 20);
    rule.enabled = false;
    assert_eq!(badge(&rule, &Counts::healthy()), None);
}

#[test]
fn a_timed_rule_is_excluded_and_until_restart_is_not() {
    let mut rule = aged("b", 20);
    for duration in ["5m", "once", "1h", ""] {
        rule.duration = duration.into();
        assert_eq!(badge(&rule, &Counts::healthy()), None, "{duration:?}");
    }
    rule.duration = "until restart".into();
    assert_eq!(badge(&rule, &Counts::healthy()), Some(HitBadge::Unused));
}

#[test]
fn rules_managed_elsewhere_get_no_badge() {
    let mut read_only = aged("b", 20);
    read_only.read_only_reason = Some("managed".into());
    let blocklist = aged("z00-blocklist:ads:domains", 20);
    let packaged = aged("000-snitchwatch-bridge-fetch", 20);
    for rule in [read_only, blocklist, packaged] {
        assert_eq!(badge(&rule, &Counts::healthy()), None, "{}", rule.name);
    }
}

#[test]
fn counting_for_less_than_the_window_says_only_no_hits_since() {
    let mut counts = Counts::healthy();
    counts.since = Some(NOW - 13 * DAY);
    assert_eq!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 13 * DAY,
            lossy: false
        })
    );
    // Exactly the window counts.
    counts.since = Some(NOW - W);
    assert_eq!(badge(&aged("b", 40), &counts), Some(HitBadge::Unused));
    counts.since = Some(NOW - W + 1);
    assert!(matches!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since { .. })
    ));
}

#[test]
fn counts_that_are_not_saved_never_say_unused() {
    let mut counts = Counts::healthy();
    counts.persistent = false;
    assert_eq!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 30 * DAY,
            lossy: false
        })
    );
    assert_eq!(
        unused(&[aged("b", 40)], &counts.view(), NOW, W),
        Vec::<String>::new()
    );
}

/// The period that counts starts at the bridge's last gap: hits lost before
/// it say nothing about the time after it.
#[test]
fn only_the_period_after_the_last_gap_counts() {
    let mut counts = Counts::healthy();
    counts.lossy = true;
    counts.last_gap = Some(NOW - 3 * DAY);
    assert_eq!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 3 * DAY,
            lossy: false
        }),
        "no hits since the gap, and nothing missed since then"
    );
    assert_eq!(
        unused(&[aged("b", 40)], &counts.view(), NOW, W),
        Vec::<String>::new()
    );
    // Exactly the window after the gap is enough; a moment less is not.
    counts.last_gap = Some(NOW - W);
    assert_eq!(badge(&aged("b", 40), &counts), Some(HitBadge::Unused));
    counts.last_gap = Some(NOW - W + 1);
    assert!(matches!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since { .. })
    ));
    // A gap long ago (one bridge restart, weeks back) costs nothing.
    counts.last_gap = Some(NOW - W - 1);
    assert_eq!(badge(&aged("b", 40), &counts), Some(HitBadge::Unused));
}

#[test]
fn the_period_starts_at_the_latest_of_counting_the_rule_and_the_gap() {
    let mut counts = Counts::healthy();
    counts.lossy = true;
    // The gap is older than the rule: the rule's age decides.
    counts.last_gap = Some(NOW - 30 * DAY);
    assert_eq!(
        badge(&aged("b", 5), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 5 * DAY,
            lossy: false
        })
    );
    // The gap is older than counting began (it can't be, but be safe).
    counts.since = Some(NOW - 2 * DAY);
    counts.last_gap = Some(NOW - 10 * DAY);
    assert_eq!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 2 * DAY,
            lossy: false
        })
    );
}

/// Without knowing when, there is no period to trust.
#[test]
fn a_gap_of_unknown_time_leaves_no_trusted_period() {
    let mut counts = Counts::healthy();
    counts.lossy = true;
    counts.last_gap = None;
    assert_eq!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 30 * DAY,
            lossy: true
        })
    );
    assert_eq!(
        unused(&[aged("b", 40)], &counts.view(), NOW, W),
        Vec::<String>::new()
    );
}

#[test]
fn a_rule_younger_than_the_window_has_not_had_the_time_to_be_used() {
    assert_eq!(
        badge(&aged("b", 3), &Counts::healthy()),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 3 * DAY,
            lossy: false
        }),
        "counted from when the rule was created"
    );
    assert_eq!(
        badge(&aged("b", 14), &Counts::healthy()),
        Some(HitBadge::Unused)
    );
}

#[test]
fn a_rule_of_unknown_age_is_never_called_unused() {
    let mut rule = aged("b", 40);
    rule.created = 0;
    assert_eq!(
        badge(&rule, &Counts::healthy()),
        Some(HitBadge::Since {
            since_unix_ms: NOW - 30 * DAY,
            lossy: false
        })
    );
}

#[test]
fn unsaved_counts_after_a_gap_say_no_hits_since_the_gap() {
    let mut counts = Counts::healthy();
    counts.persistent = false;
    counts.since = Some(NOW - 2 * DAY);
    counts.lossy = true;
    counts.last_gap = Some(NOW - DAY);
    assert_eq!(
        badge(&aged("b", 40), &counts),
        Some(HitBadge::Since {
            since_unix_ms: NOW - DAY,
            lossy: false
        })
    );
}

#[test]
fn no_counts_from_the_bridge_means_no_badge() {
    assert_eq!(
        hit_badge(&aged("b", 40), &RuleHitsView::default(), NOW, W),
        None
    );
    let mut counts = Counts::healthy();
    counts.since = None;
    assert_eq!(
        badge(&aged("b", 40), &counts),
        None,
        "counting hasn't started"
    );
}

#[test]
fn the_window_is_fourteen_days() {
    assert_eq!(W, 14 * DAY);
}

/// The bridge can't count a rule whose name is too long or has control
/// characters, so its zero says nothing: no badge at all.
#[test]
fn a_rule_the_bridge_cannot_count_is_never_unused() {
    for name in ["x".repeat(300), "bad\u{1}name".to_string()] {
        let rule = aged(&name, 20);
        assert_eq!(badge(&rule, &Counts::healthy()), None, "{name:?}");
    }
}
