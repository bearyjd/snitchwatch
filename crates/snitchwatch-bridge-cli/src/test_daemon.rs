//! Shared test fixtures: a real `DaemonCommands` whose stream said HELLO,
//! and a scripted responder standing in for opensnitchd (rule import and
//! rule command tests).

use snitchwatch_bridge::cache::rules::{RulesSync, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::{DaemonCommands, DaemonTransport, StreamRegistration};
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{
    Notification, NotificationReply, NotificationReplyCode, Operator, Rule,
};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

pub(crate) fn host_rule(name: &str, action: &str) -> Rule {
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

pub(crate) struct Daemon {
    pub(crate) commands: DaemonCommands,
    pub(crate) sync: RulesSync,
    pub(crate) cache: SharedRulesCache,
    pub(crate) registration: Option<StreamRegistration>,
    pub(crate) stream: u64,
    pub(crate) rx: mpsc::Receiver<Notification>,
    pub(crate) broadcast: broadcast::Sender<ServerMessage>,
}

/// A daemon stream that said HELLO with `rules` as its snapshot.
pub(crate) fn daemon(rules: Vec<Rule>) -> Daemon {
    daemon_on(DaemonTransport::Unix, rules)
}

pub(crate) fn daemon_on(transport: DaemonTransport, rules: Vec<Rule>) -> Daemon {
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

pub(crate) fn reply(id: u64, ok: bool, data: &str) -> NotificationReply {
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

/// What a responder saw, and the most notifications awaiting an answer.
pub(crate) type Seen = Arc<StdMutex<Vec<Notification>>>;

/// Answers every notification with `answer(rule name)` (`None`: no reply),
/// in batches once nothing new arrives for 20 ms.
pub(crate) fn respond(
    daemon: &mut Daemon,
    answer: impl Fn(&str) -> Option<(bool, String)> + Send + 'static,
) -> (Seen, Arc<StdMutex<usize>>) {
    respond_to(daemon, move |n| answer(&n.rules[0].name))
}

/// [`respond`], answering from the whole notification (its action too).
pub(crate) fn respond_to(
    daemon: &mut Daemon,
    answer: impl Fn(&Notification) -> Option<(bool, String)> + Send + 'static,
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
                        if let Some((ok, data)) = answer(&n) {
                            commands.on_reply(stream, &reply(n.id, ok, &data));
                        }
                    }
                }
            }
        }
    });
    (seen, max_outstanding)
}

pub(crate) fn names(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|n| n.rules[0].name.clone())
        .collect()
}

/// A model of opensnitchd's rule loader (`vendor/opensnitch/daemon/rule/
/// loader.go`), in its real order, for tests that depend on it:
/// - `CHANGE_RULE` (`Replace` → `replaceUserRule`): an existing `always`
///   rule changed to a temporary one loses its file first
///   (`deleteOldRuleFromDisk`); then an enabled rule is compiled, and a
///   compile error answers ERROR with the old rule left in memory;
///   otherwise the rule replaces it in memory and an `always` rule's file
///   is written (`Save`).
/// - `DELETE_RULE` (`Delete`): the rule leaves memory first; then an
///   `always` rule's file is removed, and only that can fail (ERROR).
/// - Live reload (on by default, `main.go`): a removed rule file makes the
///   watcher drop that rule from memory if it is still an `always` rule
///   (`liveReloadWorker` → `deleteRule`). Modelled as happening before the
///   command's answer.
#[derive(Debug, Default)]
pub(crate) struct LoaderModel {
    /// Rules that apply, by name.
    pub(crate) memory: std::collections::BTreeMap<String, Rule>,
    /// Rule files on disk, by name.
    pub(crate) files: std::collections::BTreeSet<String>,
    /// A rule whose conditions hold one of these values fails to compile.
    pub(crate) uncompilable: std::collections::BTreeSet<String>,
    /// Files that can't be removed.
    pub(crate) stuck_files: std::collections::BTreeSet<String>,
    /// `(action, name)` commands that get no answer (and do nothing).
    pub(crate) silent: std::collections::BTreeSet<(i32, String)>,
    /// The daemon runs with `-no-live-reload`.
    pub(crate) no_live_reload: bool,
}

pub(crate) type SharedModel = Arc<StdMutex<LoaderModel>>;

fn data_of(op: &Operator) -> Vec<String> {
    std::iter::once(op.data.clone())
        .chain(op.list.iter().flat_map(data_of))
        .collect()
}

impl LoaderModel {
    pub(crate) fn with_rules(rules: &[Rule]) -> Self {
        let mut model = Self::default();
        for rule in rules {
            if rule.duration == "always" {
                model.files.insert(rule.name.clone());
            }
            model.memory.insert(rule.name.clone(), rule.clone());
        }
        model
    }

    /// The daemon's answer to one command: `Ok(())` or ERROR text, or
    /// `None` for no answer.
    fn apply(&mut self, n: &Notification) -> Option<Result<(), String>> {
        let rule = n.rules[0].clone();
        if self.silent.contains(&(n.r#type, rule.name.clone())) {
            return None;
        }
        if n.r#type == snitchwatch_proto::protocol::Action::DeleteRule as i32 {
            let Some(old) = self.memory.remove(&rule.name) else {
                return Some(Ok(()));
            };
            if old.duration != "always" {
                return Some(Ok(()));
            }
            if self.stuck_files.contains(&rule.name) {
                return Some(Err("remove: operation not permitted".into()));
            }
            self.files.remove(&rule.name);
            return Some(Ok(()));
        }
        let mut file_removed = false;
        if let Some(old) = self.memory.get(&rule.name) {
            if old.duration == "always" && rule.duration != "always" {
                file_removed = self.files.remove(&rule.name);
            }
        }
        let fails = rule.enabled
            && rule
                .operator
                .as_ref()
                .is_some_and(|op| data_of(op).iter().any(|d| self.uncompilable.contains(d)));
        if fails {
            if file_removed {
                self.watcher_saw_removal(&rule.name);
            }
            return Some(Err("(2) error compiling rule: bad".into()));
        }
        if rule.duration == "always" {
            self.files.insert(rule.name.clone());
        }
        self.memory.insert(rule.name.clone(), rule);
        Some(Ok(()))
    }
}

impl LoaderModel {
    /// The live-reload watcher's Remove reaction (`deleteRule`).
    fn watcher_saw_removal(&mut self, name: &str) {
        let always = self
            .memory
            .get(name)
            .is_some_and(|r| r.duration == "always");
        if !self.no_live_reload && always {
            self.memory.remove(name);
        }
    }
}

/// Answer `daemon`'s commands from `model`, as opensnitchd would.
pub(crate) fn run_model(daemon: &mut Daemon, model: SharedModel) -> Seen {
    respond_to(daemon, move |n| {
        model.lock().unwrap().apply(n).map(|outcome| match outcome {
            Ok(()) => (true, String::new()),
            Err(text) => (false, text),
        })
    })
    .0
}

pub(crate) fn model(rules: &[Rule]) -> SharedModel {
    Arc::new(StdMutex::new(LoaderModel::with_rules(rules)))
}

/// Names of the rules that apply in `model`.
pub(crate) fn applying(model: &SharedModel) -> Vec<String> {
    model.lock().unwrap().memory.keys().cloned().collect()
}

/// Names of the rule files in `model`.
pub(crate) fn files(model: &SharedModel) -> Vec<String> {
    model.lock().unwrap().files.iter().cloned().collect()
}
