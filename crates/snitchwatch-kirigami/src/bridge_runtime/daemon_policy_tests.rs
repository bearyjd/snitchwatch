//! `DaemonPolicy` (capability `daemonPolicyV1`): the runtime keeps the live
//! session's latest one, only from a bridge that advertised it, and a
//! reconnect or disconnect never inherits it. (Kept out of `tests.rs`, which
//! is over the 800-line cap.)

use super::*;
use snitchwatch_bridge::rule_policy::PolicyContext;
use snitchwatch_bridge::ws_messages::DaemonPolicy;

fn handles_and_state() -> (BridgeHandles, Arc<Mutex<ConnectionState>>) {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, _inbound_rx) = mpsc::channel(8);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
        held_back: HeldBackNotices::default(),
    };
    (handles, connection)
}

fn policy(refuse_source_network: bool) -> ServerMessage {
    ServerMessage::DaemonPolicy(DaemonPolicy {
        refuse_source_network,
        daemon_version: Some(String::from(if refuse_source_network {
            "1.9"
        } else {
            "1.8"
        })),
    })
}

const REFUSES: PolicyContext = PolicyContext {
    refuse_source_network: true,
};

#[test]
fn the_capability_name_is_the_bridges() {
    assert_eq!(
        snitchwatch_bridge::bridge_capabilities::DAEMON_POLICY_V1,
        "daemonPolicyV1"
    );
}

#[tokio::test]
async fn the_live_sessions_latest_policy_is_kept_and_follows_changes() {
    let (handles, connection) = handles_and_state();
    let session = mark_connected(&connection, true);
    daemon_policy::mark_advertised(&connection, session, true);
    assert_eq!(handles.daemon_policy(), PolicyContext::default());

    daemon_policy::observe(&connection, session, &policy(true));
    assert_eq!(handles.daemon_policy(), REFUSES);
    // The daemon was replaced by an older one: the bridge re-sends.
    daemon_policy::observe(&connection, session, &policy(false));
    assert_eq!(handles.daemon_policy(), PolicyContext::default());
    // Other messages leave it alone.
    daemon_policy::observe(&connection, session, &policy(true));
    daemon_policy::observe(&connection, session, &ServerMessage::ClearConnectionRows);
    assert_eq!(handles.daemon_policy(), REFUSES);
}

/// An older bridge never sends one; one it sent anyway (it can't, but a
/// future bridge without the capability) is ignored: today's behaviour.
#[tokio::test]
async fn a_bridge_without_the_capability_keeps_the_default() {
    let (handles, connection) = handles_and_state();
    let session = mark_connected(&connection, true);
    daemon_policy::mark_advertised(&connection, session, false);
    daemon_policy::observe(&connection, session, &policy(true));
    assert_eq!(handles.daemon_policy(), PolicyContext::default());
}

#[tokio::test]
async fn a_reconnect_or_disconnect_never_inherits_the_policy() {
    let (handles, connection) = handles_and_state();
    let first = mark_connected(&connection, true);
    daemon_policy::mark_advertised(&connection, first, true);
    daemon_policy::observe(&connection, first, &policy(true));
    assert_eq!(handles.daemon_policy(), REFUSES);

    // A new session to an older bridge starts from the default, and a late
    // message from the first session doesn't reach it.
    let second = mark_connected(&connection, true);
    assert_eq!(handles.daemon_policy(), PolicyContext::default());
    daemon_policy::mark_advertised(&connection, first, true);
    daemon_policy::observe(&connection, first, &policy(true));
    assert_eq!(handles.daemon_policy(), PolicyContext::default());

    daemon_policy::mark_advertised(&connection, second, true);
    daemon_policy::observe(&connection, second, &policy(true));
    assert_eq!(handles.daemon_policy(), REFUSES);
    let (_tx, mut inbound_rx) = mpsc::channel::<QueuedClientMessage>(1);
    disconnect_and_discard(&connection, &mut inbound_rx);
    assert_eq!(handles.daemon_policy(), PolicyContext::default());
}
