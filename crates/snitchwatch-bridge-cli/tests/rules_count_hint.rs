//! The Rules page hint when the daemon's rule count disagrees (issue #65,
//! option c; plan `docs/superpowers/plans/2026-10-09-rules-count-hint-65.md`),
//! end to end: a bridge, and a mock daemon that answers commands from a model
//! of its loader and reports that loader's `NumRules()` in every ping.
//!
//! The numbers asserted are the plan's: two readings are ignored after the
//! bridge changes its list, three repeats of one disagreement raise the hint
//! and three agreeing readings clear it.

use std::time::Duration;

use mock_opensnitchd::loader::{spawn_loader_responder, LoaderModel, SharedLoader};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome, ServerMessage};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, EphemeralReason, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{
    ClientConfig, Notification, NotificationReply, NotificationReplyCode, Operator, Rule,
};
use tokio::sync::{broadcast, mpsc};

const WAIT: Duration = Duration::from_secs(10);

async fn start() -> (tempfile::TempDir, RunningBridge) {
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Ephemeral(EphemeralReason::InProcess),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
    )
    .await
    .unwrap();
    (sockets, bridge)
}

/// A rule the bridge's policy accepts: bound to a program.
fn rule(name: &str) -> Rule {
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

fn model(names: &[&str]) -> SharedLoader {
    let rules: Vec<Rule> = names.iter().map(|name| rule(name)).collect();
    LoaderModel::with_rules(&rules).shared()
}

/// The watcher loads a file: memory changes, nobody is told.
fn file_appears(model: &SharedLoader, name: &str) {
    model.lock().unwrap().memory.insert(name.into(), rule(name));
}

fn file_goes(model: &SharedLoader, name: &str) {
    model.lock().unwrap().memory.remove(name);
}

/// The daemon's end: subscribed with its loader's rules, HELLO sent, commands
/// answered from the model. Keep it alive.
struct Daemon {
    mock: MockOpensnitchd,
    model: SharedLoader,
    _seen: mpsc::Receiver<Notification>,
    _replies: mpsc::Sender<NotificationReply>,
    pings: u64,
}

impl Daemon {
    async fn connect(bridge: &RunningBridge, generation: u64, model: &SharedLoader) -> Self {
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
    async fn ping(&mut self, times: usize) {
        for _ in 0..times {
            let rules = self.model.lock().unwrap().num_rules();
            self.report(rules).await;
        }
    }

    /// A ping that carries `rules`, whatever the loader holds now.
    async fn report(&mut self, rules: u64) {
        self.pings += 1;
        self.mock
            .ping_reporting_rules(self.pings, rules, self.pings)
            .await
            .unwrap();
    }
}

/// What reached the GUIs about the rule list since the last call. (Every ping
/// also broadcasts its statistics, which is not what is asserted on here.)
fn drain(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
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

fn hint_flag(message: &ServerMessage) -> Option<bool> {
    match message {
        ServerMessage::RulesNotShown { count_mismatch, .. } => Some(*count_mismatch),
        _ => None,
    }
}

/// Every hint state the bridge announced since the last call, in order.
fn announced(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<bool> {
    drain(rx).iter().filter_map(hint_flag).collect()
}

/// The names and the hint of a snapshot answer, as a GUI that connects now
/// would get them.
async fn snapshot(
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

async fn command_result(rx: &mut broadcast::Receiver<ServerMessage>) -> RuleCommandOutcome {
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
fn delete_rule(name: &str, request_id: &str) -> ClientMessage {
    ClientMessage::DeleteRule {
        rule_id: name.into(),
        request_id: Some(request_id.into()),
        reply: None,
    }
}

/// The #65 case: the daemon's watcher loads a rule file and nothing reaches
/// the bridge. The hint appears after three pings, once; it does not touch
/// the list; it goes after three agreeing pings, once.
#[tokio::test]
async fn a_rule_file_that_appears_raises_the_hint_once_and_removing_it_clears_it() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;

    daemon.ping(15).await;
    assert!(!announced(&mut rx).contains(&true), "agreement is silent");

    file_appears(&model, "100-dropped");
    daemon.ping(2).await;
    assert!(drain(&mut rx).is_empty(), "two readings are not enough");
    daemon.ping(1).await;
    let seen = drain(&mut rx);
    assert!(
        matches!(
            seen.as_slice(),
            [ServerMessage::RulesNotShown {
                count_mismatch: true,
                listed: true,
                ..
            }]
        ),
        "{seen:?}"
    );

    daemon.ping(40).await;
    assert!(
        drain(&mut rx).is_empty(),
        "it holds without a word: no list re-sent, no flicker"
    );

    // Advice only: the list is what it was, and a GUI that comes late is told.
    let (names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(names, vec!["100-a", "100-b"]);
    assert!(hint);

    file_goes(&model, "100-dropped");
    daemon.ping(2).await;
    assert!(
        drain(&mut rx).is_empty(),
        "two agreeing readings are not enough"
    );
    daemon.ping(1).await;
    assert_eq!(announced(&mut rx), vec![false]);
    daemon.ping(40).await;
    assert!(drain(&mut rx).is_empty());
}

/// A reading built before the daemon applied (or the bridge learned of) the
/// bridge's own change is old: it never raises the hint.
#[tokio::test]
async fn a_change_the_bridge_made_never_raises_the_hint_even_with_old_readings() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b", "100-c", "100-d", "100-e"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;
    drain(&mut rx);

    for (i, name) in ["100-a", "100-b", "100-c"].into_iter().enumerate() {
        bridge
            .inbound_tx
            .send(delete_rule(name, &format!("del-{i}")))
            .await
            .unwrap();
        assert_eq!(command_result(&mut rx).await, RuleCommandOutcome::Ok);
        let before = model.lock().unwrap().num_rules() + 1;
        // Pings built just before the daemon applied it.
        daemon.report(before).await;
        daemon.report(before).await;
        daemon.report(before).await;
        daemon.ping(8).await;
    }
    assert!(!announced(&mut rx).contains(&true));
    let (names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(names, vec!["100-d", "100-e"]);
    assert!(!hint);
}

/// The daemon has applied a command whose `OK` the bridge has not seen: the
/// counts differ for as long as the reply takes, and that is not a hint.
#[tokio::test]
async fn a_command_still_waiting_for_its_reply_never_raises_the_hint() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b"]);
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
    let (replies, mut inbound) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    let mut pings = 0;
    for _ in 0..5 {
        pings += 1;
        mock.ping_reporting_rules(pings, 2, pings).await.unwrap();
    }

    bridge
        .inbound_tx
        .send(delete_rule("100-b", "slow"))
        .await
        .unwrap();
    let command = tokio::time::timeout(WAIT, inbound.recv())
        .await
        .expect("no command")
        .expect("stream closed");
    model.lock().unwrap().apply(&command).unwrap();
    // Applied, not yet answered: one is the daemon's count, two the list's.
    for _ in 0..12 {
        pings += 1;
        mock.ping_reporting_rules(pings, 1, pings).await.unwrap();
    }
    assert!(!announced(&mut rx).contains(&true));

    replies
        .send(NotificationReply {
            id: command.id,
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(command_result(&mut rx).await, RuleCommandOutcome::Ok);
    for _ in 0..12 {
        pings += 1;
        mock.ping_reporting_rules(pings, 1, pings).await.unwrap();
    }
    assert!(!announced(&mut rx).contains(&true));

    // The pause is not permanent: a file the bridge never heard of still counts.
    file_appears(&model, "100-dropped");
    for _ in 0..3 {
        pings += 1;
        mock.ping_reporting_rules(pings, 2, pings).await.unwrap();
    }
    assert_eq!(announced(&mut rx), vec![true]);
}

/// A daemon that reconnects sends its list again: that adoption is the fix
/// the hint points at, and the hint goes with it, without coming back.
#[tokio::test]
async fn a_reconnected_daemons_list_replaces_the_hint() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;
    drain(&mut rx);
    file_appears(&model, "100-dropped");
    daemon.ping(3).await;
    assert_eq!(announced(&mut rx), vec![true]);

    drop(daemon);
    let mut daemon = Daemon::connect(&bridge, 2, &model).await;
    let (names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(names, vec!["100-a", "100-b", "100-dropped"]);
    assert!(!hint, "the new list is the new baseline");
    daemon.ping(40).await;
    assert!(
        !announced(&mut rx).contains(&true),
        "and the counts agree now"
    );
}

/// The daemon sends no ping at all without new events, and a ping without
/// statistics says nothing about rules.
#[tokio::test]
async fn pings_without_statistics_say_nothing() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    file_appears(&model, "100-dropped");
    for id in 1..=30 {
        daemon.mock.ping(id).await.unwrap();
    }
    assert!(!announced(&mut rx).contains(&true));
    // A zero is not evidence either: proto3 cannot say "not reported".
    for _ in 0..30 {
        daemon.report(0).await;
    }
    assert!(!announced(&mut rx).contains(&true));
}
