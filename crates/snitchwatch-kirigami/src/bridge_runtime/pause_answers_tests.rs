//! Issue #78 in the client runtime: the `pauseAnswersWaiting` capability is
//! recorded per session, so the tray promises that a pause lets the waiting
//! prompts through only on a bridge that does it.

use super::prompt_slot_tests::handles;
use super::*;

#[tokio::test]
async fn the_pause_answers_waiting_capability_belongs_to_the_live_session() {
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = handles(&connection);
    let (_inbound_tx, mut inbound_rx) = mpsc::channel(1);

    assert!(
        !handles.advertises_pause_answers_waiting(),
        "no session yet"
    );
    mark_connected_with(&connection, false, false, true);
    assert!(handles.advertises_pause_answers_waiting());
    disconnect_and_discard(&connection, &mut inbound_rx);
    assert!(
        !handles.advertises_pause_answers_waiting(),
        "after a disconnect"
    );
    assert!(
        !connection.lock().unwrap().pause_answers_waiting,
        "a disconnect clears the flag itself"
    );
    // A reconnect to an older bridge (bare acknowledgement).
    mark_connected(&connection, true);
    assert!(!handles.advertises_pause_answers_waiting());
}
