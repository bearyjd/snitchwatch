//! Shared fixtures for the rule import tests: a real `DaemonCommands` whose
//! stream said HELLO, and a scripted responder standing in for opensnitchd.

use super::apply::Applier;
use super::Replier;
use snitchwatch_bridge::cache::rules::{RulesSync, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::{DaemonCommands, DaemonTransport, StreamRegistration};
use snitchwatch_bridge::rule_io::ImportOutcome;
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{
    Notification, NotificationReply, NotificationReplyCode, Operator, Rule,
};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

pub(super) fn host_rule(name: &str, action: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: true,
        action: action.into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: format!("{name}.example"),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(super) struct Daemon {
    pub(super) commands: DaemonCommands,
    pub(super) sync: RulesSync,
    pub(super) cache: SharedRulesCache,
    pub(super) registration: Option<StreamRegistration>,
    pub(super) stream: u64,
    pub(super) rx: mpsc::Receiver<Notification>,
    pub(super) broadcast: broadcast::Sender<ServerMessage>,
}

/// A daemon stream that said HELLO with `rules` as its snapshot.
pub(super) fn daemon(rules: Vec<Rule>) -> Daemon {
    daemon_on(DaemonTransport::Unix, rules)
}

pub(super) fn daemon_on(transport: DaemonTransport, rules: Vec<Rule>) -> Daemon {
    let (broadcast, _) = broadcast::channel(4096);
    let sync = RulesSync::new(broadcast.clone());
    let commands = DaemonCommands::new(transport, sync.clone());
    sync.stage(None, rules);
    let (registration, rx) = commands.open_stream(None);
    let stream = registration.id();
    commands.on_reply(stream, &reply(0, true, ""));
    assert!(
        !sync.cache().lock().unwrap().is_unknown(),
        "HELLO committed"
    );
    Daemon {
        commands,
        cache: sync.cache(),
        sync,
        registration: Some(registration),
        stream,
        rx,
        broadcast,
    }
}

pub(super) fn reply(id: u64, ok: bool, data: &str) -> NotificationReply {
    NotificationReply {
        id,
        code: if ok {
            NotificationReplyCode::Ok as i32
        } else {
            NotificationReplyCode::Error as i32
        },
        data: data.into(),
    }
}

pub(super) fn applier(daemon: &Daemon) -> Applier {
    Applier::new(
        daemon.commands.clone(),
        daemon.cache.clone(),
        Replier::broadcast(daemon.broadcast.clone()),
        "p".into(),
        Duration::from_secs(5),
        Duration::from_millis(10),
    )
}

/// What a responder saw, and the most notifications awaiting an answer.
pub(super) type Seen = Arc<StdMutex<Vec<Notification>>>;

/// Answers every notification with `answer(rule name)` (`None`: no reply),
/// in batches once nothing new arrives for 20 ms.
pub(super) fn respond(
    daemon: &mut Daemon,
    answer: impl Fn(&str) -> Option<(bool, String)> + Send + 'static,
) -> (Seen, Arc<StdMutex<usize>>) {
    let seen: Seen = Arc::default();
    let max_outstanding = Arc::new(StdMutex::new(0));
    let (_, placeholder) = mpsc::channel(1);
    let mut rx = std::mem::replace(&mut daemon.rx, placeholder);
    let commands = daemon.commands.clone();
    let stream = daemon.stream;
    let (seen_task, max_task) = (seen.clone(), max_outstanding.clone());
    tokio::spawn(async move {
        let mut outstanding: Vec<Notification> = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_millis(20), rx.recv()).await {
                Ok(Some(n)) => {
                    seen_task.lock().unwrap().push(n.clone());
                    outstanding.push(n);
                    let mut max = max_task.lock().unwrap();
                    *max = (*max).max(outstanding.len());
                }
                Ok(None) => return,
                Err(_) => {
                    for n in outstanding.drain(..) {
                        if let Some((ok, data)) = answer(&n.rules[0].name) {
                            commands.on_reply(stream, &reply(n.id, ok, &data));
                        }
                    }
                }
            }
        }
    });
    (seen, max_outstanding)
}

pub(super) fn progress(
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> Vec<(String, ImportOutcome)> {
    let mut out = Vec::new();
    while let Ok(message) = rx.try_recv() {
        if let ServerMessage::RulesImportProgress { name, outcome, .. } = message {
            out.push((name, outcome));
        }
    }
    out
}

pub(super) fn names(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|n| n.rules[0].name.clone())
        .collect()
}
