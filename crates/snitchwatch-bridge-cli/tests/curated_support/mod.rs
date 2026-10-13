//! Shared by `curated_defaults.rs` and `curated_dns.rs`: a system bridge with
//! a persistent state directory, driven over its inbound channel like a GUI,
//! and the mock daemon, which checks every rule the way opensnitchd compiles
//! it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::loader::{spawn_loader_responder, SharedLoader};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{ClientConfig, Notification};
use tokio::sync::{broadcast, mpsc};

pub const WAIT: Duration = Duration::from_secs(10);

pub struct Setup {
    pub _sockets: tempfile::TempDir,
    pub _state_dir: tempfile::TempDir,
    pub state: PathBuf,
    pub bridge: RunningBridge,
    pub rx: broadcast::Receiver<ServerMessage>,
}

pub async fn start(mode: BridgeMode) -> Setup {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
            mode,
        },
    )
    .await
    .unwrap();
    // Never the host's accounts: systemd-resolve is 193 here.
    bridge.set_curated_uid_lookup(std::sync::Arc::new(|name| {
        (name == "systemd-resolve").then_some(193)
    }));
    let rx = bridge.broadcast_tx.subscribe();
    Setup {
        _sockets: sockets,
        _state_dir: state_dir,
        state,
        bridge,
        rx,
    }
}

/// A daemon (re)starting with `rules` as its snapshot; returns once its
/// HELLO is the bridge's `generation`th.
pub async fn connect_daemon(
    bridge: &RunningBridge,
    generation: u64,
    rules: Vec<snitchwatch_proto::protocol::Rule>,
) -> (MockOpensnitchd, mpsc::Receiver<Notification>) {
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon
        .subscribe_with_config(ClientConfig {
            id: 1,
            name: "mock".into(),
            version: "mock-1.8.0".into(),
            rules,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_responder(ListsPolicy::Accept, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= generation))
        .await
        .expect("no HELLO")
        .unwrap();
    (daemon, seen)
}

/// A daemon (re)starting from `model`, answering as its loader would.
pub async fn connect_model(
    bridge: &RunningBridge,
    generation: u64,
    model: &SharedLoader,
) -> (MockOpensnitchd, mpsc::Receiver<Notification>) {
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    let rules = model.lock().unwrap().snapshot();
    daemon
        .subscribe_with_config(ClientConfig {
            id: 1,
            name: "mock".into(),
            version: "mock-1.8.0".into(),
            rules,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_loader_responder(model.clone(), replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= generation))
        .await
        .expect("no HELLO")
        .unwrap();
    (daemon, seen)
}

pub async fn send(bridge: &RunningBridge, msg: ClientMessage) {
    bridge.inbound_tx.send(msg).await.unwrap();
}

/// Watch `SetCuratedDefaults` until the entry `id` satisfies `done`.
pub async fn entry_until_id(
    rx: &mut broadcast::Receiver<ServerMessage>,
    id: &str,
    what: &str,
    done: impl Fn(&CuratedDefaultSummary, &Option<String>) -> bool,
) -> CuratedDefaultSummary {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::SetCuratedDefaults {
                entries,
                unavailable,
                ..
            }) = rx.recv().await
            {
                let entry = entries.into_iter().find(|e| e.id == id).unwrap();
                if done(&entry, &unavailable) {
                    return entry;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"))
}

pub async fn next_command(seen: &mut mpsc::Receiver<Notification>) -> Notification {
    tokio::time::timeout(WAIT, seen.recv())
        .await
        .expect("no command reached the daemon")
        .unwrap()
}

pub async fn nothing_sent(seen: &mut mpsc::Receiver<Notification>) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(seen.try_recv().is_err(), "a command reached the daemon");
}

/// Deliberate opt-in traverses the authenticated socket and its private ticket.
pub async fn consent_dns(bridge: &RunningBridge, purpose: &str) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let transport = tokio::net::UnixStream::connect(&bridge.ws_socket_path)
        .await
        .unwrap();
    let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", transport)
        .await
        .unwrap();
    gui.send(Message::Text(bridge.ws_token.as_str().into()))
        .await
        .unwrap();
    let ack = gui.next().await.unwrap().unwrap();
    assert!(ack.to_string().contains("authenticated"));
    let prepare = serde_json::json!({"action":"prepareCuratedBroadOptIn","requestId":"dns-test","id":"dns-resolved","purpose":purpose});
    gui.send(Message::Text(prepare.to_string())).await.unwrap();
    let ticket = tokio::time::timeout(WAIT, async {
        loop {
            let frame = gui.next().await.unwrap().unwrap();
            if let Message::Text(text) = frame {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                if value["action"] == "curatedBroadOptInPrepared" {
                    break value["ticket"].as_str().unwrap().to_owned();
                }
            }
        }
    })
    .await
    .unwrap();
    let action = if purpose == "keep" {
        "keepCuratedBroadDefault"
    } else {
        "enableCuratedBroadDefault"
    };
    let enable = serde_json::json!({"action":action,"requestId":"dns-test","id":"dns-resolved","purpose":purpose,
        "ticket":ticket,"consent":"resolver-any-address-v1"});
    gui.send(Message::Text(enable.to_string())).await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            let frame = gui.next().await.unwrap().unwrap();
            if let Message::Text(text) = frame {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                if value["action"] == "curatedDefaultsRequestResult"
                    && value["requestId"] == "dns-test"
                {
                    assert_eq!(value["results"][0]["outcome"], "choiceAccepted", "{value}");
                    assert_eq!(value["results"][0]["persisted"], true, "{value}");
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
}
