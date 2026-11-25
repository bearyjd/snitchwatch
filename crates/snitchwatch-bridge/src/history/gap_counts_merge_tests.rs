//! Coalesced outages: rows the ledger merged must never add up to more than
//! was lost (the reader pairs by overlap, not by edge equality).

use super::*;
use crate::history::ledger::GapCause;
use proptest::prelude::*;

fn row(start: i64, end: i64, cause: GapCause, count: u64) -> Gap {
    Gap {
        start_ms: start,
        end_ms: end,
        start_exact: true,
        cause,
        count,
    }
}

#[test]
fn two_merged_outages_with_one_loss_each_report_at_most_two() {
    // Pre halves merged to [100,500] (2), post halves to [300,800] (2),
    // disconnects to [200,600] (2): the true loss is 2.
    let gaps = vec![
        row(100, 500, GapCause::UpstreamLoss, 2),
        row(200, 600, GapCause::DaemonDisconnected, 2),
        row(300, 800, GapCause::UpstreamLoss, 2),
    ];
    let summary = summarize(&gaps);
    assert!(summary.lost_at_least <= 2, "{summary:?}");
    assert!(summary.lost_at_least >= 1);
    assert!(summary.may_overcount_by_pairs >= 1, "{summary:?}");
}

/// One outage as the writer records it after a ping that lost `n`: the
/// stretch from the last ping `p` to the down edge, the outage, and the
/// stretch from daemon-up to the ping at `q`.
#[derive(Debug)]
struct Layout {
    p: i64,
    d: i64,
    u: i64,
    q: i64,
    n: u64,
}

/// Rows of the same cause that overlap or touch are one row with the summed
/// count: the ledger's merge, in its widest form.
fn merged(mut rows: Vec<Gap>) -> Vec<Gap> {
    rows.sort_by_key(|g| g.start_ms);
    let mut out: Vec<Gap> = Vec::new();
    for g in rows {
        match out.last_mut() {
            Some(last) if g.start_ms <= last.end_ms => {
                last.end_ms = last.end_ms.max(g.end_ms);
                last.count += g.count;
            }
            _ => out.push(g),
        }
    }
    out
}

fn rows_of(layouts: &[Layout]) -> Vec<Gap> {
    let (mut pre, mut down, mut post) = (Vec::new(), Vec::new(), Vec::new());
    for l in layouts {
        pre.push(row(l.p, l.d, GapCause::UpstreamLoss, 1));
        down.push(row(l.d, l.u, GapCause::DaemonDisconnected, 1));
        post.push(row(
            l.u,
            l.q,
            GapCause::UpstreamLoss,
            l.n.saturating_sub(1).max(1),
        ));
    }
    // Pre and post halves are both upstream loss: the ledger merges any that
    // overlap, whichever half they were.
    let upstream = [pre, post].concat();
    [merged(upstream), merged(down)].concat()
}

fn layouts() -> impl Strategy<Value = Vec<Layout>> {
    // Outages in time order, 10..=90 apart; `p` may be shared (no ping in
    // between) and `q` may stretch over later outages.
    prop::collection::vec(
        (1i64..30, 1i64..30, 0i64..200, 1u64..6, any::<bool>()),
        1..6,
    )
    .prop_map(|steps| {
        let mut at = 1000;
        let mut last_ping = 900;
        let mut out = Vec::new();
        for (gap, len, tail, n, share) in steps {
            let (d, u) = (at + gap, at + gap + len);
            let p = if share { last_ping } else { at.min(d) - 1 };
            last_ping = p;
            out.push(Layout {
                p,
                d,
                u,
                q: u + tail,
                n,
            });
            at = u + 1;
        }
        out
    })
}

proptest! {
    #[test]
    fn merged_outages_never_report_more_than_the_truth(ls in layouts()) {
        let truth: u64 = ls.iter().map(|l| l.n).sum();
        let shown = summarize(&rows_of(&ls)).lost_at_least;
        prop_assert!(shown <= truth, "shown {shown} for truth {truth}");
    }
}
