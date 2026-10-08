//! Issue #72, end to end through `bridge_feed::dispatch_to`, the one path
//! every QML verdict takes: on a bridge session without app-bound rules, a
//! remembered "This host only" or "Any host on this domain" verdict is queued
//! once-only, whatever the sheet sent. (Kept out of `tests.rs`, which is near
//! the 800-line cap.)

use super::*;
use snitchwatch_bridge::ws_messages::{VerdictAction, VerdictDuration, VerdictScope};

fn verdict(row_id: &str, scope: VerdictScope, duration: Option<VerdictDuration>) -> ClientMessage {
    ClientMessage::SetVerdict {
        row_id: row_id.to_string(),
        verdict: VerdictAction::Allow,
        scope,
        duration,
        remember: None,
    }
}

/// The duration of the next queued verdict.
async fn queued_duration(inbound_rx: &mut mpsc::Receiver<QueuedClientMessage>) -> VerdictDuration {
    match inbound_rx
        .recv()
        .await
        .expect("a verdict was queued")
        .message
    {
        ClientMessage::SetVerdict {
            duration, remember, ..
        } => snitchwatch_bridge::ws_messages::effective_verdict_duration(duration, remember),
        other => panic!("expected SetVerdict, got {other:?}"),
    }
}

#[tokio::test]
async fn only_a_capable_session_gets_a_remembered_host_scoped_verdict() {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, mut inbound_rx) = mpsc::channel(8);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    };
    let forever = Some(VerdictDuration::Always);

    // Session 1: an old bridge (bare acknowledgement).
    mark_connected(&connection, false);
    for (scope, expected) in [
        (VerdictScope::ThisHost, VerdictDuration::Once),
        (VerdictScope::AnyHostOnDomain, VerdictDuration::Once),
        (VerdictScope::AnyHost, VerdictDuration::Always),
    ] {
        crate::bridge_feed::dispatch_to(&handles, verdict("1:7", scope, forever)).unwrap();
        assert_eq!(
            queued_duration(&mut inbound_rx).await,
            expected,
            "{scope:?}"
        );
    }
    let legacy = ClientMessage::SetVerdict {
        row_id: "1:8".to_string(),
        verdict: VerdictAction::Allow,
        scope: VerdictScope::ThisHost,
        duration: None,
        remember: Some(true),
    };
    crate::bridge_feed::dispatch_to(&handles, legacy).unwrap();
    assert_eq!(
        queued_duration(&mut inbound_rx).await,
        VerdictDuration::Once
    );

    // Session 2: a bridge that advertised app-bound rules keeps the choice.
    disconnect_and_discard(&connection, &mut inbound_rx);
    mark_connected(&connection, true);
    crate::bridge_feed::dispatch_to(&handles, verdict("2:7", VerdictScope::ThisHost, forever))
        .unwrap();
    assert_eq!(
        queued_duration(&mut inbound_rx).await,
        VerdictDuration::Always
    );
}
