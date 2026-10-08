//! Issue #73 through a whole bridge on the system service's transport: the
//! daemon dials a Unix socket, its rule snapshot is what the page counts, and
//! a request from the page deletes only the rules Snitchwatch made. (Over the
//! legacy TCP connection nothing is offered; `tests/blocklist_leftovers.rs`.)
//! The root-only peer check of the real socket is not in play here, so this
//! runs without root.

use std::time::Duration;

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_proto::protocol::{Action, ClientConfig, Operator, Rule};
use tokio::net::{UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;

use super::*;

const WAIT: Duration = Duration::from_secs(10);
const ROOT: &str = "/var/lib/snitchwatch/blocklists";

fn bridge_rule(kind: &str) -> Rule {
    Rule {
        name: format!("z00-blocklist:ads-0123456789abcdef:{kind}"),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "lists".into(),
            operand: format!("lists.{kind}"),
            data: format!("{ROOT}/ads-0123456789abcdef/{kind}"),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Someone else's rule under the blocklist prefix: not the bridge's shape.
fn foreign_rule() -> Rule {
    Rule {
        name: "z00-blocklist:foreign:domains".into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "x.example".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn next_leftovers(
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> (u32, Option<String>, Option<String>) {
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetBlocklistLeftovers {
                count,
                cause,
                reason,
            } = rx.recv().await.unwrap()
            {
                return (count, cause, reason);
            }
        }
    })
    .await
    .expect("no SetBlocklistLeftovers")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rules_nothing_manages_are_counted_with_their_cause_and_removable() {
    let dir = tempfile::tempdir().unwrap();
    let grpc_path = dir.path().join("opensnitchd.sock");
    let config = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let options = RunOptions {
        storage: Storage::Ephemeral(EphemeralReason::NotConfigured),
        blocklist_fetcher: None,
        mode: BridgeMode::System,
    };
    let bridge = run_with_incoming(
        config,
        GrpcEndpoint::Unix(grpc_path.clone()),
        UnixListenerStream::new(UnixListener::bind(&grpc_path).unwrap()),
        None,
        None,
        options,
        ANSWER_TIMEOUT,
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();

    let channel = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = grpc_path.clone();
            async move {
                UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap();
    let mut daemon = MockOpensnitchd::from_channel(channel);
    daemon
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules: vec![bridge_rule("domains"), bridge_rule("ips"), foreign_rule()],
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let mut seen = spawn_responder(ListsPolicy::Accept, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();

    // The committed snapshot is counted (the foreign rule isn't), with the
    // reason nothing manages them.
    loop {
        let (count, cause, reason) = next_leftovers(&mut rx).await;
        if count == 2 {
            assert_eq!(cause.as_deref(), Some("no_state_dir"));
            assert_eq!(reason, None);
            break;
        }
    }

    bridge
        .inbound_tx
        .send(ClientMessage::RemoveLeftoverBlocklistRules)
        .await
        .unwrap();
    let mut deleted = Vec::new();
    for _ in 0..2 {
        let n = tokio::time::timeout(WAIT, seen.recv())
            .await
            .expect("no delete reached the daemon")
            .unwrap();
        assert_eq!(n.r#type, Action::DeleteRule as i32);
        deleted.push(n.rules[0].name.clone());
    }
    deleted.sort();
    assert_eq!(
        deleted,
        vec![
            "z00-blocklist:ads-0123456789abcdef:domains".to_string(),
            "z00-blocklist:ads-0123456789abcdef:ips".to_string(),
        ]
    );
    loop {
        if next_leftovers(&mut rx).await.0 == 0 {
            break;
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), seen.recv())
            .await
            .is_err(),
        "the foreign rule was left alone"
    );
    bridge.shutdown();
}
