//! Issue #65 (the Rules page hint when the daemon's rule count disagrees)
//! through a whole bridge on the system service's transport, a Unix socket;
//! `tests/rules_count_hint.rs` covers TCP. The daemon's rule loader is the
//! shared model of `mock_opensnitchd`, and it reports its `NumRules()` in
//! every ping. The root-only peer check of the real socket is not in play
//! here, so this runs without root.

use std::time::Duration;

use mock_opensnitchd::loader::{spawn_loader_responder, LoaderModel, SharedLoader};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_proto::protocol::{ClientConfig, Notification, Operator, Rule};
use tokio::net::{UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;

use super::*;

const WAIT: Duration = Duration::from_secs(10);

fn rule(name: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A system bridge on a Unix socket, and a daemon whose loader holds `names`.
async fn start(dir: &std::path::Path, names: &[&str]) -> (RunningBridge, Pinger) {
    let grpc_path = dir.join("opensnitchd.sock");
    let bridge = run_with_incoming(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.join("bridge.sock"),
            cache_capacity: 64,
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
    let rules: Vec<Rule> = names.iter().map(|name| rule(name)).collect();
    let model = LoaderModel::with_rules(&rules).shared();
    daemon
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_loader_responder(model.clone(), replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    let pinger = Pinger {
        daemon,
        model,
        pings: 0,
        _seen: seen,
    };
    (bridge, pinger)
}

/// The daemon, pinging with its loader's count and an uptime that only grows.
struct Pinger {
    daemon: MockOpensnitchd,
    model: SharedLoader,
    pings: u64,
    /// Dropping it stands for the daemon exiting.
    _seen: mpsc::Receiver<Notification>,
}

impl Pinger {
    async fn ping(&mut self, times: u64) {
        for _ in 0..times {
            self.pings += 1;
            let rules = self.model.lock().unwrap().num_rules();
            self.daemon
                .ping_reporting_rules(self.pings, rules, self.pings)
                .await
                .unwrap();
        }
    }
}

/// The hint states announced since the last call, in order.
fn announced(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<bool> {
    let mut seen = Vec::new();
    while let Ok(message) = rx.try_recv() {
        if let ServerMessage::RulesNotShown { count_mismatch, .. } = message {
            seen.push(count_mismatch);
        }
    }
    seen
}

/// The #65 case on the system service's transport: the daemon's watcher
/// loads a rule file and nothing reaches the bridge. The hint appears after
/// three pings, once, and goes after three agreeing pings, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rule_file_that_appears_raises_the_hint_once_on_the_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (bridge, mut daemon) = start(dir.path(), &["100-a", "100-b"]).await;
    let model = daemon.model.clone();
    let mut rx = bridge.broadcast_tx.subscribe();
    daemon.ping(15).await;
    assert!(!announced(&mut rx).contains(&true), "agreement is silent");

    model
        .lock()
        .unwrap()
        .memory
        .insert("100-dropped".into(), rule("100-dropped"));
    daemon.ping(2).await;
    assert!(announced(&mut rx).is_empty(), "two readings are not enough");
    daemon.ping(1).await;
    assert_eq!(announced(&mut rx), vec![true]);
    daemon.ping(40).await;
    assert!(announced(&mut rx).is_empty(), "it holds without a word");

    model.lock().unwrap().memory.remove("100-dropped");
    daemon.ping(2).await;
    assert!(announced(&mut rx).is_empty());
    daemon.ping(1).await;
    assert_eq!(announced(&mut rx), vec![false]);
    bridge.shutdown();
}
