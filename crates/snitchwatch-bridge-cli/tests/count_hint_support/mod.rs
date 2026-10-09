//! Shared by `rules_count_hint.rs` and `rules_count_hint_unseen.rs`: a bridge,
//! and a mock daemon that answers commands from a model of its loader and
//! reports that loader's `NumRules()` in every ping.
#![allow(dead_code)]

use std::time::Duration;

use mock_opensnitchd::loader::{spawn_loader_responder, LoaderModel, SharedLoader};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome, ServerMessage};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, EphemeralReason, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{ClientConfig, Notification, NotificationReply, Operator, Rule};
use tokio::sync::{broadcast, mpsc};

pub const WAIT: Duration = Duration::from_secs(10);

/// A per-user bridge: it refuses `AddRule`, and allows toggles and deletes.
pub async fn start() -> (tempfile::TempDir, RunningBridge) {
    let (sockets, _state, bridge) = start_in(BridgeMode::User, None).await;
    (sockets, bridge)
}

/// A system bridge with a state directory, where rules may be added.
pub async fn start_system() -> (tempfile::TempDir, tempfile::TempDir, RunningBridge) {
    let (sockets, state, bridge) =
        start_in(BridgeMode::System, Some(tempfile::tempdir().unwrap())).await;
    (sockets, state.expect("a state directory"), bridge)
}

async fn start_in(
    mode: BridgeMode,
    state: Option<tempfile::TempDir>,
) -> (tempfile::TempDir, Option<tempfile::TempDir>, RunningBridge) {
    let sockets = tempfile::tempdir().unwrap();
    let storage = match &state {
        Some(dir) => Storage::Persistent(dir.path().canonicalize().unwrap()),
        None => Storage::Ephemeral(EphemeralReason::InProcess),
    };
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage,
            blocklist_fetcher: None,
            mode,
        },
    )
    .await
    .unwrap();
    (sockets, state, bridge)
}

/// A rule the bridge's policy accepts: bound to a program.
pub fn rule(name: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "list".into(),
            operand: "list".into(),
            list: vec![
                Operator {
                    r#type: "simple".into(),
                    operand: "process.path".into(),
                    data: "/usr/bin/curl".into(),
                    sensitive: true,
                    ..Default::default()
                },
                Operator {
                    r#type: "simple".into(),
                    operand: "dest.host".into(),
                    data: "example.com".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn model(names: &[&str]) -> SharedLoader {
    let rules: Vec<Rule> = names.iter().map(|name| rule(name)).collect();
    LoaderModel::with_rules(&rules).shared()
}

/// The watcher loads a file: memory changes, nobody is told.
pub fn file_appears(model: &SharedLoader, name: &str) {
    model.lock().unwrap().memory.insert(name.into(), rule(name));
}

pub fn file_goes(model: &SharedLoader, name: &str) {
    model.lock().unwrap().memory.remove(name);
}

/// The daemon's end: subscribed with its loader's rules, HELLO sent, commands
/// answered from the model. Keep it alive.
pub struct Daemon {
    pub mock: MockOpensnitchd,
    pub model: SharedLoader,
    _seen: mpsc::Receiver<Notification>,
    _replies: mpsc::Sender<NotificationReply>,
    pings: u64,
}

impl Daemon {
    pub async fn connect(bridge: &RunningBridge, generation: u64, model: &SharedLoader) -> Self {
        let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
            .await
            .unwrap();
        let rules = model.lock().unwrap().snapshot();
        mock.subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules,
            ..Default::default()
        })
        .await
        .unwrap();
        let (replies, inbound) = mock.open_notifications().await.unwrap();
        let seen = spawn_loader_responder(model.clone(), replies.clone(), inbound);
        let mut ready = bridge.daemon_stream_ready();
        tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= generation))
            .await
            .expect("no HELLO")
            .unwrap();
        Self {
            mock,
            model: model.clone(),
            _seen: seen,
            _replies: replies,
            pings: 0,
        }
    }

    /// A ping as the daemon builds it: its loader's current count.
    pub async fn ping(&mut self, times: usize) {
        for _ in 0..times {
            let rules = self.model.lock().unwrap().num_rules();
            self.report(rules).await;
        }
    }

    /// A ping that carries `rules`, whatever the loader holds now.
    pub async fn report(&mut self, rules: u64) {
        self.pings += 1;
        self.mock
            .ping_reporting_rules(self.pings, rules, self.pings)
            .await
            .unwrap();
    }
}

/// What reached the GUIs about the rule list since the last call. (Every ping
/// also broadcasts its statistics, which is not what is asserted on here.)
pub fn drain(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
    let mut seen = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(
                message @ (ServerMessage::SetRules { .. }
                | ServerMessage::UpdateRules { .. }
                | ServerMessage::RulesNotShown { .. }),
            ) => seen.push(message),
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => return seen,
        }
    }
}

pub fn hint_flag(message: &ServerMessage) -> Option<bool> {
    match message {
        ServerMessage::RulesNotShown { count_mismatch, .. } => Some(*count_mismatch),
        _ => None,
    }
}

/// Every hint state the bridge announced since the last call, in order.
pub fn announced(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<bool> {
    drain(rx).iter().filter_map(hint_flag).collect()
}

/// The names and the hint of a snapshot answer, as a GUI that connects now
/// would get them.
pub async fn snapshot(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> (Vec<String>, bool) {
    drain(rx);
    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        let mut names = None;
        loop {
            match rx.recv().await {
                Ok(ServerMessage::SetRules { rules }) => {
                    names = Some(
                        rules
                            .iter()
                            .map(|rule| rule["name"].as_str().unwrap().to_string())
                            .collect(),
                    )
                }
                Ok(ServerMessage::RulesNotShown { count_mismatch, .. }) => {
                    if let Some(names) = names.take() {
                        return (names, count_mismatch);
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .expect("no snapshot answer")
}

pub async fn command_result(rx: &mut broadcast::Receiver<ServerMessage>) -> RuleCommandOutcome {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::RuleCommandResult { outcome, .. }) = rx.recv().await {
                return outcome;
            }
        }
    })
    .await
    .expect("no rule command result")
}

/// A GUI-made change that works in the per-user setup (adding rules needs
/// the system service) and moves the daemon's count.
pub fn delete_rule(name: &str, request_id: &str) -> ClientMessage {
    ClientMessage::DeleteRule {
        rule_id: name.into(),
        request_id: Some(request_id.into()),
        reply: None,
    }
}

pub fn add_rule(rule: &Rule, request_id: &str) -> ClientMessage {
    ClientMessage::AddRule {
        rule: snitchwatch_bridge::rule_wire::rule_to_wire(rule),
        request_id: Some(request_id.into()),
        reply: None,
    }
}
