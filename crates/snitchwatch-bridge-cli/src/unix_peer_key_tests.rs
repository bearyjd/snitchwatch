//! Issue #391: every connection to the per-user Unix socket is its own peer,
//! as every TCP connection is. Observed through the daemon-version gate: a
//! peer that does not hold the current `Notifications` stream cannot move the
//! recorded version (`observe_daemon_version`).

use std::time::Duration;

use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::daemon_config::DaemonVersion;
use snitchwatch_proto::protocol::Statistics;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

use super::*;

const WAIT: Duration = Duration::from_secs(10);

fn stats(version: &str) -> Statistics {
    Statistics {
        daemon_version: version.into(),
        ..Default::default()
    }
}

/// Dropping the stream halves closes the `Notifications` stream.
struct Holder {
    daemon: MockOpensnitchd,
    _stream: (
        tokio::sync::mpsc::Sender<snitchwatch_proto::protocol::NotificationReply>,
        tokio::sync::mpsc::Receiver<snitchwatch_proto::protocol::Notification>,
    ),
}

/// A daemon that has subscribed and said HELLO, so it holds the stream.
async fn holder(bridge: &RunningBridge, version: &str) -> Holder {
    let mut daemon = MockOpensnitchd::connect_unix(bridge.grpc_endpoint.unix_path().unwrap())
        .await
        .unwrap()
        .with_version(version);
    daemon.subscribe("holder").await.unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    Holder {
        daemon,
        _stream: (replies, inbound),
    }
}

async fn user_bridge(dir: &std::path::Path) -> RunningBridge {
    run(BridgeConfig::user_unix_for_tests(dir)).await.unwrap()
}

async fn ping(daemon: &mut MockOpensnitchd, id: u64, version: &str) {
    daemon.ping_with_stats(id, stats(version)).await.unwrap();
}

#[tokio::test]
async fn a_second_connection_to_the_per_user_socket_cannot_move_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = user_bridge(dir.path()).await;
    let mut a = holder(&bridge, "1.9.0").await;
    ping(&mut a.daemon, 1, "1.9.0").await;
    assert_eq!(bridge.daemon_version.get(), Some(DaemonVersion::new(1, 9)));

    let mut b = MockOpensnitchd::connect_unix(bridge.grpc_endpoint.unix_path().unwrap())
        .await
        .unwrap();
    ping(&mut b, 1, "1.8.0").await;
    assert_eq!(
        bridge.daemon_version.get(),
        Some(DaemonVersion::new(1, 9)),
        "peer B does not hold the stream"
    );
    // The holder still moves it.
    ping(&mut a.daemon, 2, "1.10.0").await;
    assert_eq!(bridge.daemon_version.get(), Some(DaemonVersion::new(1, 10)));
}

/// The system socket keeps ONE shared key (`None`): its redial relies on it
/// (plan 2026-11-19-391, section 3). `RootUnixIncoming` yields a plain
/// `UnixStream`, whose connect info carries no `UnixPeerInfo`. This stands in
/// for it end to end (the real one admits only root): the second connection
/// is the same peer, so its ping is taken.
#[tokio::test]
async fn the_system_socket_peers_share_one_key() {
    let dir = tempfile::tempdir().unwrap();
    let grpc_path = dir.path().join("opensnitchd.sock");
    let bridge = run_with_incoming(
        BridgeConfig {
            cache_capacity: 64,
            ..BridgeConfig::user_unix_for_tests(dir.path())
        },
        GrpcEndpoint::Unix(grpc_path.clone()),
        UnixListenerStream::new(UnixListener::bind(&grpc_path).unwrap()),
        None,
        None,
        RunOptions {
            storage: Storage::Ephemeral(EphemeralReason::NotConfigured),
            blocklist_fetcher: None,
            mode: BridgeMode::System,
        },
        ANSWER_TIMEOUT,
    )
    .await
    .unwrap();
    let mut a = holder(&bridge, "1.9.0").await;
    ping(&mut a.daemon, 1, "1.9.0").await;
    let mut b = MockOpensnitchd::connect_unix(&grpc_path).await.unwrap();
    ping(&mut b, 1, "1.8.0").await;
    assert_eq!(bridge.daemon_version.get(), Some(DaemonVersion::new(1, 8)));
}
