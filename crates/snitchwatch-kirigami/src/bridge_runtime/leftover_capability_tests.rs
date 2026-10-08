//! Issue #73 in the client runtime: a bridge that doesn't say it reports
//! leftover blocklist rules never says "none left", so the count an earlier
//! bridge sent is cleared when connecting to it.

use super::*;

fn received(rx: &mut broadcast::Receiver<ReceivedServerMessage>) -> Option<ReceivedServerMessage> {
    rx.try_recv().ok()
}

#[test]
fn a_bridge_without_the_capability_clears_an_earlier_leftover_count() {
    let (tx, mut rx) = broadcast::channel(4);
    clear_unreported_leftovers(&[], 7, &tx);
    let message = received(&mut rx).expect("the count is cleared");
    assert_eq!(message.connection_id, 7);
    assert_eq!(
        message.message,
        ServerMessage::SetBlocklistLeftovers {
            count: 0,
            cause: None,
            reason: None
        }
    );
}

#[test]
fn a_bridge_with_it_is_believed() {
    let (tx, mut rx) = broadcast::channel(4);
    let capabilities = vec![
        "ruleHits".to_string(),
        snitchwatch_bridge::bridge_capabilities::BLOCKLIST_LEFTOVERS.to_string(),
    ];
    clear_unreported_leftovers(&capabilities, 7, &tx);
    assert!(received(&mut rx).is_none());
}
