//! What a ping says the bridge never received (history PR 7): the lower bound
//! `max(0, Δrule_hits - received)` and whether the daemon's counters reset.

use super::*;
use snitchwatch_proto::protocol::{Event, Rule};

const T1: i64 = 1_800_000_000_000;
const T2: i64 = T1 + 5_000;

fn hit(name: &str) -> Event {
    Event {
        rule: Some(Rule {
            name: name.to_string(),
            ..Default::default()
        }),
        unixnano: 1,
        ..Default::default()
    }
}

fn events(n: usize) -> Vec<Event> {
    (0..n).map(|_| hit("a")).collect()
}

/// A hits state whose baseline is a ping at `T1` with daemon counter `10`.
fn baseline() -> RuleHits {
    let mut hits = RuleHits::default();
    let (_, loss) = hits.record_with_loss(&events(0), 100, 10, T1, |_| true);
    assert_eq!(
        loss,
        UpstreamLoss {
            at_ms: Some(T1),
            ..UpstreamLoss::default()
        },
        "the first ping has no past"
    );
    hits
}

#[test]
fn the_missing_count_is_the_counter_growth_beyond_the_events_received() {
    let mut hits = baseline();
    let (_, loss) = hits.record_with_loss(&events(3), 105, 17, T2, |_| true);
    assert_eq!(
        loss,
        UpstreamLoss {
            missing: 4,
            restarted: false,
            since_ms: Some(T1),
            at_ms: Some(T2)
        }
    );
}

#[test]
fn a_ping_that_accounts_for_every_hit_loses_nothing() {
    let mut hits = baseline();
    let (_, loss) = hits.record_with_loss(&events(5), 105, 15, T2, |_| true);
    assert_eq!((loss.missing, loss.restarted), (0, false));
}

#[test]
fn more_events_than_the_counter_grew_is_never_a_negative_loss() {
    let mut hits = baseline();
    let (_, loss) = hits.record_with_loss(&events(5), 105, 12, T2, |_| true);
    assert_eq!((loss.missing, loss.restarted), (0, false));
}

#[test]
fn a_counter_that_went_down_is_a_restart_with_no_countable_loss() {
    let mut hits = baseline();
    let (_, loss) = hits.record_with_loss(&events(1), 105, 3, T2, |_| true);
    assert_eq!(
        loss,
        UpstreamLoss {
            missing: 0,
            restarted: true,
            since_ms: Some(T1),
            at_ms: Some(T2)
        }
    );
}

#[test]
fn an_uptime_that_went_down_is_a_restart() {
    let mut hits = baseline();
    let (_, loss) = hits.record_with_loss(&events(2), 4, 12, T2, |_| true);
    assert!(loss.restarted);
}

#[test]
fn record_still_answers_only_whether_anything_changed() {
    let mut hits = RuleHits::default();
    assert!(hits.record(&events(1), 100, 1, T1, |_| true));
}
