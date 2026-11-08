//! `DaemonPolicy` (`bridge_capabilities::DAEMON_POLICY_V1`): sent right
//! after `Authenticated`, re-sent when the daemon's version changes, and
//! only then.

use super::tests::{connect, default_handles};
use super::*;
use crate::daemon_config::{DaemonVersion, SharedDaemonVersion};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TMessage;

type Client = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

/// A server whose connections read `version`, and one authenticated
/// client past its acknowledgement.
async fn authenticated_client(
    dir: &tempfile::TempDir,
    version: SharedDaemonVersion,
) -> (Client, broadcast::Sender<ServerMessage>) {
    let handles = WsHandles {
        daemon_version: version,
        ..default_handles()
    };
    let broadcast = handles.broadcast.clone();
    let token = Token::generate();
    let path = dir.path().join("bridge.sock");
    let server = WsServer::new(path.clone(), token.clone(), handles);
    let listener = server.bind().await.unwrap();
    tokio::spawn(server.serve(listener));
    let mut ws = connect(&path).await;
    ws.send(TMessage::Text(token.as_str().into()))
        .await
        .unwrap();
    let ack = next_message(&mut ws).await;
    assert!(
        matches!(ack, ServerMessage::Authenticated { .. }),
        "{ack:?}"
    );
    (ws, broadcast)
}

async fn next_message(ws: &mut Client) -> ServerMessage {
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("no frame in time")
        .expect("stream ended")
        .expect("frame error");
    serde_json::from_str(&frame.into_text().unwrap()).unwrap()
}

fn policy(version: Option<DaemonVersion>) -> ServerMessage {
    ServerMessage::DaemonPolicy(DaemonPolicy::for_daemon(version))
}

#[tokio::test]
async fn daemon_policy_is_the_frame_right_after_the_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let version = SharedDaemonVersion::default();
    version.set(DaemonVersion::parse("1.9.0"));
    let (mut ws, broadcast) = authenticated_client(&dir, version).await;
    let first = next_message(&mut ws).await;
    assert_eq!(first, policy(Some(DaemonVersion::new(1, 9))));
    let ServerMessage::DaemonPolicy(sent) = first else {
        unreachable!()
    };
    assert!(sent.refuse_source_network);
    assert_eq!(sent.daemon_version.as_deref(), Some("1.9"));
    broadcast.send(ServerMessage::ClearConnectionRows).unwrap();
    assert_eq!(
        next_message(&mut ws).await,
        ServerMessage::ClearConnectionRows
    );
}

#[tokio::test]
async fn daemon_policy_is_resent_when_the_daemon_version_changes() {
    let dir = tempfile::tempdir().unwrap();
    let version = SharedDaemonVersion::default();
    let (mut ws, _broadcast) = authenticated_client(&dir, version.clone()).await;
    assert_eq!(next_message(&mut ws).await, policy(None));
    version.observe(DaemonVersion::parse("v1.9.0"));
    assert_eq!(
        next_message(&mut ws).await,
        policy(Some(DaemonVersion::new(1, 9)))
    );
    version.observe(DaemonVersion::parse("1.8.0"));
    assert_eq!(
        next_message(&mut ws).await,
        policy(Some(DaemonVersion::new(1, 8)))
    );
    // The stream closed: the version is forgotten, and that is a change too.
    version.set(None);
    assert_eq!(next_message(&mut ws).await, policy(None));
}

/// Unchanged writes queue nothing: the next frame after them is the one
/// for a real change (a duplicate would sit ahead of it in the queue), and
/// a broadcast sent meanwhile is the only other frame.
#[tokio::test]
async fn daemon_policy_is_not_resent_when_the_version_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let version = SharedDaemonVersion::default();
    version.set(DaemonVersion::parse("1.8.0"));
    let (mut ws, broadcast) = authenticated_client(&dir, version.clone()).await;
    let eight = policy(Some(DaemonVersion::new(1, 8)));
    assert_eq!(next_message(&mut ws).await, eight);
    version.set(DaemonVersion::parse("1.8.0"));
    version.set(DaemonVersion::parse("mock-1.8.2"));
    version.observe(DaemonVersion::parse("mock-1.8.5"));
    version.observe(DaemonVersion::parse("garbage"));
    broadcast.send(ServerMessage::ClearConnectionRows).unwrap();
    assert_eq!(
        next_message(&mut ws).await,
        ServerMessage::ClearConnectionRows
    );
    let quiet = tokio::time::timeout(Duration::from_millis(200), ws.next()).await;
    assert!(quiet.is_err(), "unexpected frame: {quiet:?}");
    version.set(DaemonVersion::parse("1.9.0"));
    assert_eq!(
        next_message(&mut ws).await,
        policy(Some(DaemonVersion::new(1, 9)))
    );
}
