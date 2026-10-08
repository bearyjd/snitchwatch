//! The import task: routing, preview state, refusals, where answers go, and
//! how an apply ends.

use super::test_support::*;
use super::*;
use snitchwatch_bridge::daemon_commands::DaemonTransport;
use snitchwatch_bridge::ws_messages::ReplyTo;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

fn document(rules: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "format": "snitchwatch.rules", "version": 1, "rules": rules })
}

fn is_import_answer(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::RulesExport { .. }
            | ServerMessage::RulesExportUnavailable { .. }
            | ServerMessage::RulesImportPreview { .. }
            | ServerMessage::RulesImportRefused { .. }
            | ServerMessage::RulesImportResult { .. }
    )
}

async fn next_import_message(rx: &mut broadcast::Receiver<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = rx.recv().await.unwrap();
            if is_import_answer(&message) {
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
            busy: crate::busy::BusyNames::default(),
        },
    )
}

fn preview_message(rules: Vec<serde_json::Value>) -> ClientMessage {
    ClientMessage::PreviewRulesImport {
        request_id: "r-preview".into(),
        document: document(rules),
        reply: None,
    }
}

async fn preview_id(import: &RulesImport, rx: &mut broadcast::Receiver<ServerMessage>) -> String {
    let rule = snitchwatch_bridge::rule_io::export_rule(&host_rule("new", "deny"));
    assert!(import.try_route(preview_message(vec![rule])).is_none());
    match next_import_message(rx).await {
        ServerMessage::RulesImportPreview {
            preview_id,
            items,
            request_id,
        } => {
            assert_eq!(items.len(), 1);
            assert_eq!(request_id, "r-preview");
            preview_id
        }
        other => panic!("expected a preview, got {other:?}"),
    }
}

fn apply_message(preview_id: &str) -> ClientMessage {
    ClientMessage::ApplyRulesImport {
        request_id: "r-apply".into(),
        preview_id: preview_id.into(),
        include: vec!["new".into()],
        reply: None,
    }
}

fn export_message() -> ClientMessage {
    ClientMessage::ExportRules {
        request_id: "r-export".into(),
        reply: None,
    }
}

async fn refusal(rx: &mut broadcast::Receiver<ServerMessage>) -> String {
    match next_import_message(rx).await {
        ServerMessage::RulesImportRefused { reason, .. } => reason,
        other => panic!("expected a refusal, got {other:?}"),
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

    assert!(import.try_route(export_message()).is_none());
    match next_import_message(&mut rx).await {
        ServerMessage::RulesExport {
            document,
            request_id,
            ..
        } => {
            assert_eq!(request_id, "r-export");
            let names: Vec<_> = document.rules.iter().map(|r| r["name"].clone()).collect();
            assert_eq!(names, vec!["a", "b"]);
        }
        other => panic!("expected an export, got {other:?}"),
    }

    daemon.cache.lock().unwrap().set_unknown();
    import.try_route(export_message());
    assert!(matches!(
        next_import_message(&mut rx).await,
        ServerMessage::RulesExportUnavailable { .. }
    ));
}

/// Review M5: on the legacy TCP transport any local process can pose as the
/// daemon (#35), so nothing is exported, previewed or applied there.
#[tokio::test]
async fn import_and_export_are_refused_on_the_tcp_transport() {
    let daemon = daemon_on(DaemonTransport::Tcp, vec![host_rule("a", "deny")]);
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();

    import.try_route(export_message());
    match next_import_message(&mut rx).await {
        ServerMessage::RulesExportUnavailable { reason, .. } => assert_eq!(reason, TCP_REFUSED),
        other => panic!("{other:?}"),
    }
    let rule = snitchwatch_bridge::rule_io::export_rule(&host_rule("new", "deny"));
    import.try_route(preview_message(vec![rule]));
    assert_eq!(refusal(&mut rx).await, TCP_REFUSED);
    import.try_route(apply_message("any"));
    assert_eq!(refusal(&mut rx).await, TCP_REFUSED);
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
    assert_eq!(refusal(&mut rx).await, UNKNOWN_PREVIEW);

    // The expiry tick prunes the rule between preview and apply.
    let id = preview_id(&import, &mut rx).await;
    let tick = tokio::spawn(snitchwatch_bridge::cache::rules::prune_expired_rules_every(
        Duration::from_millis(10),
        Arc::downgrade(&daemon.cache),
        daemon.broadcast.clone(),
        daemon.sync.hits(),
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
    assert_eq!(refusal(&mut rx).await, STALE_PREVIEW);

    // A fresh preview applies, once.
    let id = preview_id(&import, &mut rx).await;
    import.try_route(apply_message(&id));
    match next_import_message(&mut rx).await {
        ServerMessage::RulesImportResult {
            applied,
            preview_id,
            ..
        } => assert_eq!((applied, preview_id.as_str()), (1, id.as_str())),
        other => panic!("{other:?}"),
    }
    import.try_route(apply_message(&id));
    assert_eq!(refusal(&mut rx).await, UNKNOWN_PREVIEW);
}

#[tokio::test]
async fn a_preview_expires() {
    let daemon = daemon(Vec::new());
    let import = task(&daemon, Duration::from_millis(50));
    let mut rx = daemon.broadcast.subscribe();
    let id = preview_id(&import, &mut rx).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    import.try_route(apply_message(&id));
    assert_eq!(refusal(&mut rx).await, UNKNOWN_PREVIEW);
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
    assert_eq!(refusal(&mut rx).await, IMPORT_RUNNING);
}

#[tokio::test]
async fn an_apply_of_more_than_2000_rules_is_refused_and_the_preview_kept() {
    let mut daemon = daemon(Vec::new());
    respond(&mut daemon, |_| Some((true, String::new())));
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();
    let rules: Vec<_> = (0..=MAX_APPLY_RULES)
        .map(|i| snitchwatch_bridge::rule_io::export_rule(&host_rule(&format!("r{i:05}"), "deny")))
        .collect();
    import.try_route(preview_message(rules));
    let ServerMessage::RulesImportPreview {
        preview_id, items, ..
    } = next_import_message(&mut rx).await
    else {
        panic!("no preview")
    };
    let all: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
    let apply = |include: Vec<String>| ClientMessage::ApplyRulesImport {
        request_id: "r".into(),
        preview_id: preview_id.clone(),
        include,
        reply: None,
    };
    import.try_route(apply(all.clone()));
    assert_eq!(refusal(&mut rx).await, TOO_MANY_AT_ONCE);
    import.try_route(apply(all[..2].to_vec()));
    assert!(matches!(
        next_import_message(&mut rx).await,
        ServerMessage::RulesImportResult { applied: 2, .. }
    ));
}

#[tokio::test]
async fn a_refused_document_is_reported_with_a_fixed_reason() {
    let daemon = daemon(Vec::new());
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();
    let mut newer = document(Vec::new());
    newer["version"] = serde_json::json!(2);
    import.try_route(ClientMessage::PreviewRulesImport {
        request_id: "r".into(),
        document: newer,
        reply: None,
    });
    assert_eq!(
        refusal(&mut rx).await,
        "This file was made by a newer Snitchwatch."
    );
}

/// Review #8: answers go to the asking connection only, with its request id.
#[tokio::test]
async fn answers_go_only_to_the_requesting_connection() {
    let daemon = daemon(vec![host_rule("a", "deny")]);
    let import = task(&daemon, Duration::from_secs(600));
    let mut everyone = daemon.broadcast.subscribe();
    let (tx, mut mine) = mpsc::channel(8);
    import.try_route(ClientMessage::ExportRules {
        request_id: "mine".into(),
        reply: Some(ReplyTo::new(tx)),
    });
    let answer = tokio::time::timeout(Duration::from_secs(5), mine.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(answer, ServerMessage::RulesExport { ref request_id, .. } if request_id == "mine"),
        "{answer:?}"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    while let Ok(message) = everyone.try_recv() {
        assert!(!is_import_answer(&message), "broadcast: {message:?}");
    }
}

/// Review #5: an apply's confirmed rules reach the GUIs as one `SetRules`,
/// sent when the apply ends.
#[tokio::test]
async fn an_apply_publishes_the_rule_list_once() {
    let mut daemon = daemon(Vec::new());
    respond(&mut daemon, |_| Some((true, String::new())));
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();
    let rules: Vec<_> = (0..20)
        .map(|i| snitchwatch_bridge::rule_io::export_rule(&host_rule(&format!("r{i:02}"), "deny")))
        .collect();
    import.try_route(preview_message(rules));
    let ServerMessage::RulesImportPreview {
        preview_id, items, ..
    } = next_import_message(&mut rx).await
    else {
        panic!("no preview")
    };
    import.try_route(ClientMessage::ApplyRulesImport {
        request_id: "r".into(),
        preview_id,
        include: items.iter().map(|i| i.name.clone()).collect(),
        reply: None,
    });
    let mut set_rules = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            ServerMessage::SetRules { rules } => {
                set_rules += 1;
                assert_eq!(rules.len(), 20, "the list once, complete");
            }
            ServerMessage::RulesImportResult { applied, .. } => {
                assert_eq!(applied, 20);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(set_rules, 1);
}

/// Review #7: however an apply ends (here: its task is dropped mid-way),
/// the import is freed, the held rule list is published, and a result is
/// sent with what is known.
#[tokio::test]
async fn an_apply_that_ends_early_still_frees_the_task_and_reports() {
    let mut daemon = daemon(Vec::new());
    crate::test_daemon::respond(&mut daemon, |_| Some((true, String::new())));
    let mut rx = daemon.broadcast.subscribe();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut guard = ApplyRun::new(
        running.clone(),
        daemon.commands.hold_rule_publishes(),
        Replier::broadcast(daemon.broadcast.clone()),
        "p".into(),
    );
    // One rule confirmed while the list is held (PR #105 re-review: a hold
    // publishes only a list a confirmed command changed).
    let change = snitchwatch_proto::protocol::Notification {
        r#type: snitchwatch_proto::protocol::Action::ChangeRule as i32,
        rules: vec![crate::test_daemon::host_rule("100-x", "allow")],
        ..Default::default()
    };
    let sent = daemon.commands.send(change).unwrap();
    sent.wait(Duration::from_secs(5)).await.unwrap();
    guard.totals.applied = 3;
    drop(guard);
    assert!(!running.load(std::sync::atomic::Ordering::SeqCst));
    // The result goes out from a task of its own, off the dropping stack.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut saw_rules = false;
    let mut saw_result = false;
    while let Ok(message) = rx.try_recv() {
        match message {
            ServerMessage::SetRules { .. } => saw_rules = true,
            ServerMessage::RulesImportResult {
                applied,
                preview_id,
                ..
            } => {
                assert_eq!((applied, preview_id.as_str()), (3, "p"));
                saw_result = true;
            }
            _ => {}
        }
    }
    assert!(saw_rules && saw_result);
}

/// An apply that confirmed nothing frees the task and reports, and
/// publishes no rule list: nothing in it changed.
#[tokio::test]
async fn an_apply_that_confirmed_nothing_publishes_no_rule_list() {
    let daemon = daemon(Vec::new());
    let mut rx = daemon.broadcast.subscribe();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let guard = ApplyRun::new(
        running.clone(),
        daemon.commands.hold_rule_publishes(),
        Replier::broadcast(daemon.broadcast.clone()),
        "p".into(),
    );
    drop(guard);
    assert!(!running.load(std::sync::atomic::Ordering::SeqCst));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut saw_result = false;
    while let Ok(message) = rx.try_recv() {
        assert!(
            !matches!(message, ServerMessage::SetRules { .. }),
            "published"
        );
        saw_result |= matches!(message, ServerMessage::RulesImportResult { .. });
    }
    assert!(saw_result);
}

/// A GUI that stops reading its answers costs one wait, not one per
/// message: an apply's progress can't stall the import (and the held rule
/// list) for everyone else.
#[tokio::test(start_paused = true)]
async fn a_gui_that_stops_reading_is_given_up_on_after_one_wait() {
    let (tx, _never_read) = mpsc::channel(1);
    let (broadcast, _) = broadcast::channel(4);
    let replier = Replier::new(Some(ReplyTo::new(tx)), broadcast);
    let started = tokio::time::Instant::now();
    for i in 0..50 {
        replier
            .send(ServerMessage::RulesImportRefused {
                request_id: format!("r{i}"),
                reason: String::new(),
            })
            .await;
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// Re-review: the give-up is per connection, not per request, so a GUI
/// flooding requests and reading none costs one wait in all.
#[tokio::test(start_paused = true)]
async fn a_stalled_connection_is_given_up_on_once_across_requests() {
    let (tx, _never_read) = mpsc::channel(1);
    let connection = ReplyTo::new(tx);
    let (broadcast, _) = broadcast::channel(4);
    let started = tokio::time::Instant::now();
    for i in 0..10 {
        let replier = Replier::new(Some(connection.clone()), broadcast.clone());
        for j in 0..5 {
            replier
                .send(ServerMessage::RulesImportRefused {
                    request_id: format!("r{i}-{j}"),
                    reason: String::new(),
                })
                .await;
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// Re-review: an apply's result waits (briefly) for room on its
/// connection's queue rather than being dropped when the queue is full.
#[tokio::test]
async fn an_apply_result_waits_for_room_on_a_full_queue() {
    let daemon = daemon(Vec::new());
    let (tx, mut rx) = mpsc::channel(1);
    let connection = ReplyTo::new(tx);
    assert!(
        connection
            .send(ServerMessage::RulesImportRefused {
                request_id: "filler".into(),
                reason: String::new(),
            })
            .await
    );
    let run = ApplyRun::new(
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
        daemon.commands.hold_rule_publishes(),
        Replier::new(Some(connection), daemon.broadcast.clone()),
        "p".into(),
    );
    let finished = tokio::spawn(run.finish());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        rx.recv().await,
        Some(ServerMessage::RulesImportRefused { .. })
    ));
    finished.await.unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(ServerMessage::RulesImportResult { .. })
    ));
}

/// Import uses the same request-id rule as rule commands: an unusable id
/// is answered without echoing it.
#[tokio::test]
async fn an_unusable_request_id_is_not_echoed() {
    let daemon = daemon(Vec::new());
    let import = task(&daemon, Duration::from_secs(600));
    let mut rx = daemon.broadcast.subscribe();
    import.try_route(ClientMessage::ExportRules {
        request_id: "a/<b>".into(),
        reply: None,
    });
    match next_import_message(&mut rx).await {
        ServerMessage::RulesExport { request_id, .. } => assert_eq!(request_id, ""),
        other => panic!("{other:?}"),
    }
}
