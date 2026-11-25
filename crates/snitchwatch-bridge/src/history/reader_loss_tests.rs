//! A page that trims its ledger still reports a lost-item lower bound drawn
//! from every row of its window, not just the rows it carries.

use super::ledger::GapCause;
use super::reader::*;
use super::reader_fixture::Fx;

/// One lost item whose outage row is hidden by an `uncertain` span, plus more
/// unrelated rows than a page may carry. The page keeps the newest rows, so
/// the hiding span is the oldest and is the one dropped.
fn hidden_outage_under_a_crowd(fx: &mut Fx, crowd: usize) {
    fx.live_session_start_ms = Some(10);
    fx.now_ms = 5_000;
    fx.session(10);
    fx.events([4_000]);
    fx.gap(50, 150, GapCause::Uncertain, 1);
    fx.gap(95, 100, GapCause::UpstreamLoss, 1);
    fx.gap(110, 120, GapCause::UpstreamLoss, 1);
    for i in 0..crowd {
        let at = 1_000 + 10 * i as i64;
        fx.gap(at, at, GapCause::Disabled, 1);
    }
}

#[test]
fn a_trim_that_drops_the_hiding_row_does_not_double_a_loss_of_one() {
    let mut fx = Fx::new();
    hidden_outage_under_a_crowd(&mut fx, 98);
    let page = fx.page(None, 10);
    assert_eq!(page.partial, Some(Partial::Gaps), "the ledger was trimmed");
    assert!(
        !page.gaps.iter().any(|g| g.cause == "uncertain"),
        "the hiding row is the one dropped"
    );
    assert!(
        page.summary.lost_at_least <= 1,
        "a loss of 1 read as {}",
        page.summary.lost_at_least
    );
}
