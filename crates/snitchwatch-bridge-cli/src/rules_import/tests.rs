//! Tests for [`super`]: the apply engine against a real `DaemonCommands`
//! (its per-stream queue, replies and stream close are the real ones), and
//! the task's preview state.

use super::apply::{self, Applier, Totals};
use super::*;
use snitchwatch_bridge::cache::rules::RulesSync;
use snitchwatch_bridge::daemon_commands::{DaemonTransport, StreamRegistration};
use snitchwatch_bridge::rule_io::ImportOutcome;
use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Operator, Rule,
};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

fn host_rule(name: &str, action: &str) -> Rule {
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

struct Daemon {
    commands: DaemonCommands,
    cache: SharedRulesCache,
    registration: Option<StreamRegistration>,
    stream: u64,
    rx: mpsc::Receiver<Notification>,
    broadcast: broadcast::Sender<ServerMessage>,
}

/// A daemon stream that said HELLO with `rules` as its snapshot.
fn daemon(rules: Vec<Rule>) -> Daemon {
    let (broadcast, _) = broadcast::channel(4096);
    let sync = RulesSync::new(broadcast.clone());
    let commands = DaemonCommands::new(DaemonTransport::Unix, sync.clone());
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
        registration: Some(registration),
        stream,
        rx,
        broadcast,
    }
}

fn reply(id: u64, ok: bool, data: &str) -> NotificationReply {
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

fn applier(daemon: &Daemon) -> Applier {
    Applier {
        commands: daemon.commands.clone(),
        broadcast: daemon.broadcast.clone(),
        reply_timeout: Duration::from_secs(5),
        retry_delay: Duration::from_millis(10),
    }
}

/// Answers every notification with `answer(rule name)` (`None`: no reply),
/// recording what it saw and the most notifications awaiting an answer at
/// once. Replies in batches once nothing new arrives for 20 ms.
type Seen = Arc<StdMutex<Vec<Notification>>>;

fn respond(
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

fn progress(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<(String, ImportOutcome)> {
    let mut out = Vec::new();
    while let Ok(message) = rx.try_recv() {
        if let ServerMessage::RulesImportProgress { name, outcome } = message {
            out.push((name, outcome));
        }
    }
    out
}

// --- Apply engine ---------------------------------------------------------

#[tokio::test]
async fn each_rule_goes_out_alone_as_change_rule_in_name_order() {
    let mut daemon = daemon(vec![host_rule("kept", "deny")]);
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    let mut rx = daemon.broadcast.subscribe();
    let rules = vec![
        host_rule("b", "deny"),
        host_rule("a", "allow"),
        host_rule("c", "deny"),
    ];

    let totals = apply::run(&applier(&daemon), rules).await;

    assert_eq!(
        totals,
        Totals {
            applied: 3,
            ..Default::default()
        }
    );
    let seen = seen.lock().unwrap().clone();
    let names: Vec<_> = seen.iter().map(|n| n.rules[0].name.clone()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
    for n in &seen {
        assert_eq!(n.r#type, Action::ChangeRule as i32, "only CHANGE_RULE");
        assert_eq!(n.rules.len(), 1, "one rule per notification");
    }
    let outcomes = progress(&mut rx);
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|(_, o)| *o == ImportOutcome::Applied));
    let cached = daemon.cache.lock().unwrap().rules().unwrap().clone();
    assert!(["a", "b", "c", "kept"]
        .iter()
        .all(|n| cached.contains_key(*n)));
}

#[tokio::test]
async fn a_rejected_rule_reports_the_sanitized_daemon_text_and_the_rest_continue() {
    let mut daemon = daemon(Vec::new());
    respond(&mut daemon, |name| {
        Some(match name {
            "b" => (false, "bad <b>regexp</b>\u{202e}".to_string()),
            _ => (true, String::new()),
        })
    });
    let mut rx = daemon.broadcast.subscribe();
    let rules = vec![
        host_rule("a", "deny"),
        host_rule("b", "deny"),
        host_rule("c", "deny"),
    ];

    let totals = apply::run(&applier(&daemon), rules).await;

    assert_eq!(
        totals,
        Totals {
            applied: 2,
            rejected: 1,
            ..Default::default()
        }
    );
    let outcomes = progress(&mut rx);
    let rejected = outcomes.iter().find(|(n, _)| n == "b").unwrap();
    assert_eq!(
        rejected.1,
        ImportOutcome::Rejected {
            reason: "bad &lt;b&gt;regexp&lt;/b&gt;".into()
        }
    );
    let cached = daemon.cache.lock().unwrap().rules().unwrap().clone();
    assert!(cached.contains_key("a") && cached.contains_key("c") && !cached.contains_key("b"));
}

#[tokio::test]
async fn never_more_than_eight_rules_are_in_flight() {
    let mut daemon = daemon(Vec::new());
    let (seen, max_outstanding) = respond(&mut daemon, |_| Some((true, String::new())));
    let rules: Vec<_> = (0..40)
        .map(|i| host_rule(&format!("r{i:02}"), "deny"))
        .collect();

    let totals = apply::run(&applier(&daemon), rules).await;

    assert_eq!(totals.applied, 40);
    assert_eq!(seen.lock().unwrap().len(), 40);
    assert_eq!(*max_outstanding.lock().unwrap(), apply::MAX_IN_FLIGHT);
}

#[tokio::test]
async fn a_full_queue_is_retried_then_reported_busy_and_the_next_rule_goes_out() {
    let mut daemon = daemon(Vec::new());
    // Fill the stream's queue: nobody reads it yet.
    let mut fillers = Vec::new();
    while let Ok(pending) = daemon.commands.send(Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![host_rule("filler", "deny")],
        ..Default::default()
    }) {
        fillers.push(pending);
    }
    drop(fillers);
    let mut rx = daemon.broadcast.subscribe();
    let applier = applier(&daemon);
    let rules = vec![host_rule("a", "deny"), host_rule("b", "deny")];
    let run = tokio::spawn(async move { apply::run(&applier, rules).await });

    // Once "a" is reported busy, drain the queue and answer what follows.
    let busy = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(ServerMessage::RulesImportProgress { name, outcome }) = rx.recv().await {
                return (name, outcome);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(busy.0, "a");
    assert!(matches!(busy.1, ImportOutcome::NotSent { .. }), "{busy:?}");
    while daemon.rx.try_recv().is_ok() {}
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));

    let totals = run.await.unwrap();
    assert_eq!(
        totals,
        Totals {
            applied: 1,
            not_sent: 1,
            ..Default::default()
        }
    );
    let names: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| n.rules[0].name.clone())
        .collect();
    assert_eq!(names, vec!["b"]);
}

#[tokio::test]
async fn a_closed_stream_stops_the_import_and_the_rest_are_not_sent() {
    let mut daemon = daemon(Vec::new());
    let mut rx = daemon.broadcast.subscribe();
    let applier = applier(&daemon);
    let rules: Vec<_> = (0..12)
        .map(|i| host_rule(&format!("r{i:02}"), "deny"))
        .collect();
    let run = tokio::spawn(async move { apply::run(&applier, rules).await });

    // The first notification arrives; then the daemon's stream goes away.
    tokio::time::timeout(Duration::from_secs(5), daemon.rx.recv())
        .await
        .unwrap()
        .unwrap();
    drop(daemon.registration.take());

    let totals = run.await.unwrap();
    assert_eq!(totals.applied, 0);
    assert_eq!(totals.no_answer, apply::MAX_IN_FLIGHT as u32, "{totals:?}");
    assert_eq!(
        totals.not_sent,
        12 - apply::MAX_IN_FLIGHT as u32,
        "{totals:?}"
    );
    assert_eq!(progress(&mut rx).len(), 12, "one outcome per rule");
}

#[tokio::test]
async fn without_a_daemon_nothing_is_sent() {
    let mut daemon = daemon(Vec::new());
    drop(daemon.registration.take());
    let totals = apply::run(
        &applier(&daemon),
        vec![host_rule("a", "deny"), host_rule("b", "deny")],
    )
    .await;
    assert_eq!(
        totals,
        Totals {
            not_sent: 2,
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn an_unanswered_rule_is_reported_as_no_answer() {
    let mut daemon = daemon(Vec::new());
    respond(&mut daemon, |name| {
        (name != "a").then(|| (true, String::new()))
    });
    let applier = Applier {
        reply_timeout: Duration::from_millis(100),
        ..applier(&daemon)
    };
    let totals = apply::run(
        &applier,
        vec![host_rule("a", "deny"), host_rule("b", "deny")],
    )
    .await;
    assert_eq!(
        totals,
        Totals {
            applied: 1,
            no_answer: 1,
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn a_rule_the_policy_refuses_at_apply_time_is_not_sent() {
    let mut daemon = daemon(Vec::new());
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    let mut lists = host_rule("a", "deny");
    lists.operator = Some(Operator {
        r#type: "lists".into(),
        operand: "lists.domains".into(),
        data: "/etc".into(),
        ..Default::default()
    });
    let mut reserved = host_rule("z00-blocklist:x:domains", "allow");
    reserved.duration = "always".into();
    let mut curated = host_rule("snitchwatch-default-x", "allow");
    curated.duration = "always".into();

    let totals = apply::run(
        &applier(&daemon),
        vec![lists, reserved, curated, host_rule("ok", "deny")],
    )
    .await;

    assert_eq!(
        totals,
        Totals {
            applied: 1,
            rejected: 3,
            ..Default::default()
        }
    );
    let names: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| n.rules[0].name.clone())
        .collect();
    assert_eq!(names, vec!["ok"]);
}

// --- The import task ------------------------------------------------------

fn document(rules: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "format": "snitchwatch.rules", "version": 1, "rules": rules })
}

async fn next_import_message(rx: &mut broadcast::Receiver<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = rx.recv().await.unwrap();
            if matches!(
                message,
                ServerMessage::RulesExport { .. }
                    | ServerMessage::RulesExportUnavailable { .. }
                    | ServerMessage::RulesImportPreview { .. }
                    | ServerMessage::RulesImportRefused { .. }
                    | ServerMessage::RulesImportResult { .. }
            ) {
                return message;
            }
        }
    })
    .await
    .expect("no import message")
}

fn task(daemon: &Daemon, preview_ttl: Duration) -> RulesImport {
    RulesImport::spawn_with(
        daemon.commands.clone(),
        daemon.cache.clone(),
        daemon.broadcast.clone(),
        ImportConfig {
            reply_timeout: Duration::from_secs(5),
            retry_delay: Duration::from_millis(10),
            preview_ttl,
        },
    )
}

async fn preview_id(import: &RulesImport, rx: &mut broadcast::Receiver<ServerMessage>) -> String {
    let rule = snitchwatch_bridge::rule_io::export_rule(&host_rule("new", "deny"));
    assert!(import
        .try_route(ClientMessage::PreviewRulesImport {
            document: document(vec![rule]),
        })
        .is_none());
    match next_import_message(rx).await {
        ServerMessage::RulesImportPreview { preview_id, items } => {
            assert_eq!(items.len(), 1);
            preview_id
        }
        other => panic!("expected a preview, got {other:?}"),
    }
}

fn apply_message(preview_id: &str) -> ClientMessage {
    ClientMessage::ApplyRulesImport {
        preview_id: preview_id.into(),
        include: vec!["new".into()],
    }
}

#[tokio::test]
async fn other_messages_pass_through_the_router() {
    let daemon = daemon(Vec::new());
    let import = task(&daemon, Duration::from_secs(600));
    assert_eq!(
        import.try_route(ClientMessage::Undo),
        Some(ClientMessage::Undo)
    );
}

#[tokio::test]
async fn export_is_unavailable_until_rules_load_then_lists_them() {
    let daemon = daemon(vec![host_rule("b", "deny"), host_rule("a", "allow")]);
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();

    assert!(import.try_route(ClientMessage::ExportRules).is_none());
    match next_import_message(&mut rx).await {
        ServerMessage::RulesExport { document, .. } => {
            let names: Vec<_> = document.rules.iter().map(|r| r["name"].clone()).collect();
            assert_eq!(names, vec!["a", "b"]);
        }
        other => panic!("expected an export, got {other:?}"),
    }

    daemon.cache.lock().unwrap().set_unknown();
    import.try_route(ClientMessage::ExportRules);
    assert!(matches!(
        next_import_message(&mut rx).await,
        ServerMessage::RulesExportUnavailable { .. }
    ));
}

#[tokio::test]
async fn apply_is_refused_for_an_unknown_preview_or_after_any_rule_change() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // A five-minute rule the daemon created six minutes ago.
    let temp = Rule {
        duration: "5m".into(),
        created: now - 360,
        ..host_rule("temp", "deny")
    };
    let mut daemon = daemon(vec![temp]);
    respond(&mut daemon, |_| Some((true, String::new())));
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();

    import.try_route(apply_message("nope"));
    assert!(matches!(
        next_import_message(&mut rx).await,
        ServerMessage::RulesImportRefused { .. }
    ));

    // The expiry tick prunes the rule between preview and apply.
    let id = preview_id(&import, &mut rx).await;
    let tick = tokio::spawn(snitchwatch_bridge::cache::rules::prune_expired_rules_every(
        Duration::from_millis(10),
        Arc::downgrade(&daemon.cache),
        daemon.broadcast.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon
            .cache
            .lock()
            .unwrap()
            .rules()
            .unwrap()
            .contains_key("temp")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the expiry tick never pruned the rule");
    tick.abort();
    import.try_route(apply_message(&id));
    match next_import_message(&mut rx).await {
        ServerMessage::RulesImportRefused { reason } => assert_eq!(reason, STALE_PREVIEW),
        other => panic!("expected a refusal, got {other:?}"),
    }

    // A fresh preview applies, once.
    let id = preview_id(&import, &mut rx).await;
    import.try_route(apply_message(&id));
    assert!(matches!(
        next_import_message(&mut rx).await,
        ServerMessage::RulesImportResult { applied: 1, .. }
    ));
    import.try_route(apply_message(&id));
    assert!(matches!(
        next_import_message(&mut rx).await,
        ServerMessage::RulesImportRefused { .. }
    ));
}

#[tokio::test]
async fn a_preview_expires() {
    let daemon = daemon(Vec::new());
    let import = task(&daemon, Duration::from_millis(50));
    let mut rx = daemon.broadcast.subscribe();
    let id = preview_id(&import, &mut rx).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    import.try_route(apply_message(&id));
    match next_import_message(&mut rx).await {
        ServerMessage::RulesImportRefused { reason } => assert_eq!(reason, UNKNOWN_PREVIEW),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_second_apply_is_refused_while_one_runs() {
    let daemon = daemon(Vec::new());
    // Nobody answers: the first apply waits on its reply.
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();
    let first = preview_id(&import, &mut rx).await;
    import.try_route(apply_message(&first));
    let second = preview_id(&import, &mut rx).await;
    import.try_route(apply_message(&second));
    match next_import_message(&mut rx).await {
        ServerMessage::RulesImportRefused { reason } => assert_eq!(reason, IMPORT_RUNNING),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_document_is_reported_with_a_fixed_reason() {
    let daemon = daemon(Vec::new());
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();
    let mut newer = document(Vec::new());
    newer["version"] = serde_json::json!(2);
    import.try_route(ClientMessage::PreviewRulesImport { document: newer });
    match next_import_message(&mut rx).await {
        ServerMessage::RulesImportRefused { reason } => {
            assert_eq!(reason, "This file was made by a newer Snitchwatch.")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}
