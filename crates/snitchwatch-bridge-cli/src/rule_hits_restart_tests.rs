//! N3 (issue #117, plan `2026-10-09-n3-unused-window-from-daemon-counters.md`)
//! through whole bridges: one bridge counts a ping and shuts down, a second
//! starts on the same state directory, and the daemon (`mock_opensnitchd`,
//! keeping its counters) dials it again. On the system bridge's transport (a
//! Unix socket; the root-only peer check of the real socket is not in play)
//! that restart is judged from the daemon's counters; over TCP it is always a
//! gap.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_proto::protocol::{ClientConfig, Event, Notification, Rule, Statistics};
use tokio::net::{UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;

use super::*;

const WAIT: Duration = Duration::from_secs(10);

fn rule_a() -> Rule {
    Rule {
        name: "a".into(),
        enabled: true,
        action: "allow".into(),
        duration: "always".into(),
        ..Default::default()
    }
}

fn hit_a() -> Event {
    Event {
        rule: Some(rule_a()),
        unixnano: 1_700_000_000_000_000_000,
        ..Default::default()
    }
}

/// A daemon run: when it started, so each ping's `uptime` is real.
struct DaemonRun(SystemTime);

impl DaemonRun {
    fn started_secs_ago(secs: u64) -> Self {
        Self(SystemTime::now() - Duration::from_secs(secs))
    }

    fn stats(&self, rule_hits: u64, hits: usize) -> Statistics {
        Statistics {
            uptime: self.0.elapsed().unwrap().as_secs(),
            rule_hits,
            events: (0..hits).map(|_| hit_a()).collect(),
            ..Default::default()
        }
    }
}

struct Run {
    bridge: RunningBridge,
    rx: broadcast::Receiver<ServerMessage>,
    daemon: MockOpensnitchd,
    _commands: tokio::sync::mpsc::Receiver<Notification>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    Unix,
    Tcp,
}

fn config(dir: &Path, n: u32) -> BridgeConfig {
    BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.join(format!("bridge{n}.sock")),
        cache_capacity: 64,
    }
}

/// Bridge number `n` on `state`, with the daemon subscribed (rule `a`) and
/// its stream ready.
async fn start(transport: Transport, dir: &Path, state: &Path, n: u32) -> Run {
    let options = RunOptions {
        storage: Storage::Persistent(state.to_path_buf()),
        blocklist_fetcher: None,
        mode: match transport {
            Transport::Unix => BridgeMode::System,
            Transport::Tcp => BridgeMode::User,
        },
    };
    let (bridge, mut daemon) = match transport {
        Transport::Unix => {
            let grpc_path = dir.join(format!("opensnitchd{n}.sock"));
            let bridge = run_with_incoming(
                config(dir, n),
                GrpcEndpoint::Unix(grpc_path.clone()),
                UnixListenerStream::new(UnixListener::bind(&grpc_path).unwrap()),
                None,
                None,
                options,
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
            (bridge, MockOpensnitchd::from_channel(channel))
        }
        Transport::Tcp => {
            let bridge = run_with_options(config(dir, n), options).await.unwrap();
            let addr = bridge.grpc_endpoint.tcp_addr().unwrap();
            let daemon = MockOpensnitchd::connect(addr).await.unwrap();
            (bridge, daemon)
        }
    };
    let rx = bridge.broadcast_tx.subscribe();
    daemon
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules: vec![rule_a()],
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let commands = spawn_responder(ListsPolicy::Accept, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    Run {
        bridge,
        rx,
        daemon,
        _commands: commands,
    }
}

/// `(lossy, last gap, count of a)` from a snapshot answer.
async fn rule_hits(run: &mut Run) -> (bool, Option<i64>, Option<u64>) {
    while run.rx.try_recv().is_ok() {}
    run.bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::RuleHits {
                lossy,
                last_gap_unix_ms,
                hits,
                ..
            }) = run.rx.recv().await
            {
                let a = hits.iter().find(|h| h.name == "a").map(|h| h.count);
                return (lossy, last_gap_unix_ms, a);
            }
        }
    })
    .await
    .expect("no RuleHits in the snapshot answer")
}

/// Run one: rule `a` hit once (the daemon's counter reads 7), then a clean
/// shutdown. Returns the state directory's holder.
async fn first_run(transport: Transport, dir: &Path, state: &Path, daemon: &DaemonRun) {
    let mut run = start(transport, dir, state, 1).await;
    run.daemon
        .ping_with_stats(1, daemon.stats(7, 1))
        .await
        .unwrap();
    assert_eq!(rule_hits(&mut run).await, (false, None, Some(1)));
    run.bridge.shutdown();
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn dirs() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let state = state.canonicalize().unwrap();
    (dir, state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bridge_restart_with_the_daemon_up_keeps_the_unused_window_on_the_unix_socket() {
    let (dir, state) = dirs();
    let daemon = DaemonRun::started_secs_ago(100);
    first_run(Transport::Unix, dir.path(), &state, &daemon).await;

    let mut run = start(Transport::Unix, dir.path(), &state, 2).await;
    let (lossy, gap, a) = rule_hits(&mut run).await;
    assert!(lossy && gap.is_some(), "provisional until the first ping");
    assert_eq!(a, Some(1), "restored");
    // Two hits while no bridge was up wait in the daemon's batch.
    run.daemon
        .ping_with_stats(2, daemon.stats(9, 2))
        .await
        .unwrap();
    assert_eq!(rule_hits(&mut run).await, (false, None, Some(3)));
    run.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bridge_restart_with_lost_hits_is_a_gap_on_the_unix_socket() {
    let (dir, state) = dirs();
    let daemon = DaemonRun::started_secs_ago(100);
    first_run(Transport::Unix, dir.path(), &state, &daemon).await;

    let mut run = start(Transport::Unix, dir.path(), &state, 2).await;
    let before = now_ms();
    // Five hits, two of them still in the batch: three were lost.
    run.daemon
        .ping_with_stats(2, daemon.stats(12, 2))
        .await
        .unwrap();
    let (lossy, gap, a) = rule_hits(&mut run).await;
    assert!(lossy);
    assert!(gap.unwrap() >= before, "a gap at the first ping");
    assert_eq!(a, Some(3));
    run.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reboot_after_a_clean_stop_is_no_gap_on_the_unix_socket() {
    let (dir, state) = dirs();
    first_run(
        Transport::Unix,
        dir.path(),
        &state,
        &DaemonRun::started_secs_ago(100),
    )
    .await;

    // The daemon restarted too: its counters start again.
    let rebooted = DaemonRun::started_secs_ago(20);
    let mut run = start(Transport::Unix, dir.path(), &state, 2).await;
    run.daemon
        .ping_with_stats(2, rebooted.stats(1, 1))
        .await
        .unwrap();
    assert_eq!(rule_hits(&mut run).await, (false, None, Some(2)));
    run.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bridge_restart_over_tcp_is_a_gap() {
    let (dir, state) = dirs();
    let daemon = DaemonRun::started_secs_ago(100);
    first_run(Transport::Tcp, dir.path(), &state, &daemon).await;

    let mut run = start(Transport::Tcp, dir.path(), &state, 2).await;
    run.daemon
        .ping_with_stats(2, daemon.stats(9, 2))
        .await
        .unwrap();
    let (lossy, gap, a) = rule_hits(&mut run).await;
    assert!(
        lossy && gap.is_some(),
        "anyone local could have sent that ping"
    );
    assert_eq!(a, Some(3));
    run.bridge.shutdown();
}
