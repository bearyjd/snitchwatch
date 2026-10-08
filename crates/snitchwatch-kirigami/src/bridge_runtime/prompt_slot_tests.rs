//! Prompt-slot plan, part A, in the client runtime: the `promptSlot`
//! capability is recorded per session (so the GUI falls back to its own
//! estimate on an older bridge), and the session's latest `PromptSlot` reaches
//! the shell's watch channel labelled with its session.

use super::*;
use snitchwatch_bridge::prompt_slot::PromptSlotHolder;

fn handles(connection: &Arc<Mutex<ConnectionState>>) -> BridgeHandles {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, _inbound_rx) = mpsc::channel(1);
    BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    }
}

#[tokio::test]
async fn the_prompt_slot_capability_belongs_to_the_live_session() {
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = handles(&connection);
    let (_inbound_tx, mut inbound_rx) = mpsc::channel(1);

    mark_connected_with(&connection, false, true);
    assert!(handles.advertises_prompt_slot());
    disconnect_and_discard(&connection, &mut inbound_rx);
    assert!(!handles.advertises_prompt_slot(), "after a disconnect");
    assert!(
        !connection.lock().unwrap().prompt_slot,
        "a disconnect clears the flag itself"
    );
    // A reconnect to an older bridge (bare acknowledgement).
    mark_connected(&connection, true);
    assert!(!handles.advertises_prompt_slot());
}

#[test]
fn only_a_prompt_slot_message_reaches_the_slot_channel_with_its_session() {
    let (slot_tx, slot_rx) = watch::channel(ReceivedPromptSlot::default());
    forward_prompt_slot(&ServerMessage::ClearConnectionRows, 3, &slot_tx);
    assert_eq!(
        slot_rx.borrow().connection_id,
        0,
        "other messages are ignored"
    );

    let holder = PromptSlotHolder {
        row_id: "ask-7".into(),
        what: "steam → example.com".into(),
        since_ms: 1_000,
    };
    forward_prompt_slot(
        &ServerMessage::PromptSlot {
            holder: Some(holder.clone()),
            holders: 2,
            defaulted_at_least: Some(4),
        },
        3,
        &slot_tx,
    );
    let received = slot_rx.borrow().clone();
    assert_eq!(received.connection_id, 3);
    assert_eq!(received.holder, Some(holder));
    assert_eq!(received.holders, 2);
    assert_eq!(received.defaulted_at_least, Some(4));
}
