//! Prompt-slot plan Part C through the gRPC service: `subscribe` keeps the
//! daemon's own settings (item 10); a prompt nobody answers is answered "no
//! answer" after the timeout (S1 = P-a); "Decide later" blocks the program
//! for 5 minutes, or falls back to P-a without a bindable path (S2 = P-c).

use super::*;
use crate::cache::rules::RulesCache;
use crate::deferred_answers::{decide_later, DecidedLater, ANSWER_TIMEOUT};
use crate::ws_messages::{AutoAnswer, ConnectionRow, VerdictDuration, VerdictScope};
use tonic::Code;

type Ask = tokio::task::JoinHandle<Result<Response<Rule>, Status>>;

fn service() -> (
    UiService,
    Arc<Mutex<ConnectionCache>>,
    broadcast::Receiver<ServerMessage>,
) {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, rx) = broadcast::channel(64);
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    // Synced and empty, so a remembered verdict would show up in it.
    svc.rules_handle().lock().unwrap().replace_all(Vec::new());
    (svc, cache, rx)
}

async fn subscribe_with(svc: &UiService, default_action: &str) {
    svc.subscribe(Request::new(ClientConfig {
        config: format!(r#"{{"DefaultAction": "{default_action}"}}"#),
        ..Default::default()
    }))
    .await
    .unwrap();
}

fn spawn_ask(svc: &UiService, process_path: &str) -> Ask {
    let asking = svc.clone();
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: process_path.into(),
        ..Default::default()
    };
    tokio::spawn(async move { asking.ask_rule(Request::new(conn)).await })
}

/// The row of the next `InsertConnectionRows`.
async fn inserted(rx: &mut broadcast::Receiver<ServerMessage>) -> ConnectionRow {
    loop {
        if let ServerMessage::InsertConnectionRows { rows } = rx.recv().await.unwrap() {
            return rows[0].clone();
        }
    }
}

/// Every message broadcast and not yet read.
fn drain(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn updated_rows(messages: &[ServerMessage]) -> Vec<ConnectionRow> {
    messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::UpdateConnectionRows { rows } => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn assert_nothing_saved(svc: &UiService, messages: &[ServerMessage]) {
    assert!(
        !messages
            .iter()
            .any(|m| matches!(m, ServerMessage::UpdateRules { .. })),
        "no rule may be saved: {messages:?}"
    );
    assert_eq!(
        *svc.rules_handle().lock().unwrap(),
        RulesCache::Synced(Default::default())
    );
}

fn assert_slot_released(messages: &[ServerMessage]) {
    let last_slot = messages.iter().rev().find_map(|m| match m {
        ServerMessage::PromptSlot { holders, .. } => Some(*holders),
        _ => None,
    });
    assert_eq!(last_slot, Some(0), "the prompt slot is free again");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn the_answer_timeout_is_thirty_seconds() {
    assert_eq!(ANSWER_TIMEOUT, Duration::from_secs(30));
}

#[tokio::test]
async fn subscribe_keeps_the_daemons_settings_and_echoes_the_config() {
    let (svc, _cache, _rx) = service();
    assert_eq!(svc.daemon_config_handle().get(), None);

    let raw = r#"{"DefaultAction": "deny", "Stats": {"MaxEvents": 50}}"#;
    let echoed = svc
        .subscribe(Request::new(ClientConfig {
            config: raw.into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(echoed.config, raw, "the daemon gets its own config back");
    let view = svc.daemon_config_handle().get().unwrap();
    assert_eq!(view.default_action.as_deref(), Some("deny"));
    assert_eq!(view.max_events, Some(50));
    assert_eq!(view.checksums_enabled, None);

    // A later subscribe with garbage replaces the view: nothing is known.
    svc.subscribe(Request::new(ClientConfig {
        config: "garbage".into(),
        ..Default::default()
    }))
    .await
    .unwrap();
    assert_eq!(svc.daemon_config_handle().default_row_action(), None);
}

#[tokio::test(start_paused = true)]
async fn a_prompt_nobody_answers_gets_no_answer_after_the_timeout() {
    let (svc, cache, mut rx) = service();
    subscribe_with(&svc, "deny").await;
    let _gui = svc.client_presence().authenticated_session();
    let before = now_ms();
    let ask = spawn_ask(&svc, "/usr/bin/curl");
    let pending = inserted(&mut rx).await;
    let deadline = pending.answer_deadline_ms.expect("a countdown");
    assert!(
        (before + 30_000..=now_ms() + 30_000).contains(&deadline),
        "{deadline} is not 30 s from insertion"
    );

    tokio::time::advance(ANSWER_TIMEOUT - Duration::from_millis(1)).await;
    assert!(!ask.is_finished(), "answered before the timeout");

    let status = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("still waiting after the timeout")
        .unwrap()
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(status.message(), "no answer");

    let messages = drain(&mut rx);
    assert_nothing_saved(&svc, &messages);
    assert_slot_released(&messages);
    let updated = updated_rows(&messages);
    assert_eq!(updated.len(), 1, "{messages:?}");
    let row = &updated[0];
    assert_eq!(row.id, pending.id);
    assert!(row.deferred);
    assert_eq!(row.auto_answer, Some(AutoAnswer::NoAnswer));
    assert_eq!(
        row.action.as_deref(),
        Some("deny"),
        "the daemon's DefaultAction"
    );
    assert_eq!(row.answer_deadline_ms, None, "no countdown once answered");
    assert_eq!(row.matched_rule, None, "no rule governs it");
    let cache = cache.lock().await;
    assert_eq!(cache.pending_count(), 0);
    assert_eq!(
        cache.rows(),
        std::slice::from_ref(row),
        "the row stays listed"
    );
}

#[tokio::test(start_paused = true)]
async fn without_the_daemons_config_the_timed_out_row_names_no_action() {
    let (svc, _cache, mut rx) = service();
    let _gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, "/usr/bin/curl");
    inserted(&mut rx).await;
    let status = tokio::time::timeout(Duration::from_secs(60), ask)
        .await
        .expect("still waiting after the timeout")
        .unwrap()
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    let updated = updated_rows(&drain(&mut rx));
    assert_eq!(updated.len(), 1);
    assert!(updated[0].deferred);
    assert_eq!(
        updated[0].action, None,
        "the bridge doesn't know the action"
    );
}

#[tokio::test(start_paused = true)]
async fn an_answer_just_before_the_timeout_wins() {
    let (svc, cache, mut rx) = service();
    subscribe_with(&svc, "deny").await;
    let _gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, "/usr/bin/curl");
    let pending = inserted(&mut rx).await;

    tokio::time::advance(ANSWER_TIMEOUT - Duration::from_millis(1)).await;
    cache
        .lock()
        .await
        .resolve(
            &pending.id,
            crate::cache::connections::Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    let rule = tokio::time::timeout(Duration::from_secs(1), ask)
        .await
        .expect("not answered at once")
        .unwrap()
        .unwrap()
        .into_inner();
    assert_eq!(rule.action, "allow");

    // Well past the deadline nothing settles it again.
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert!(updated_rows(&drain(&mut rx)).is_empty());
    let cache = cache.lock().await;
    let row = &cache.rows()[0];
    assert_eq!(row.action.as_deref(), Some("allow"));
    assert!(!row.deferred);
    assert_eq!(row.auto_answer, None);
    assert_eq!(
        row.answer_deadline_ms, None,
        "the countdown ends with the answer"
    );
}

#[tokio::test(start_paused = true)]
async fn decide_later_blocks_a_bindable_program_for_five_minutes() {
    let (svc, cache, mut rx) = service();
    subscribe_with(&svc, "allow").await;
    let _gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, "/usr/bin/curl");
    let pending = inserted(&mut rx).await;

    let outcome = decide_later(
        &cache,
        &svc.daemon_config_handle(),
        &svc.broadcast,
        &pending.id,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DecidedLater::BlockedForFiveMinutes);

    let rule = tokio::time::timeout(Duration::from_secs(1), ask)
        .await
        .expect("not answered at once")
        .unwrap()
        .unwrap()
        .into_inner();
    assert_eq!(rule.action, "deny");
    assert_eq!(rule.duration, "5m");
    let operator = rule.operator.unwrap();
    assert_eq!(
        (
            operator.r#type.as_str(),
            operator.operand.as_str(),
            operator.data.as_str()
        ),
        ("simple", "process.path", "/usr/bin/curl"),
        "the program alone, any host"
    );

    let messages = drain(&mut rx);
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, ServerMessage::UpdateRules { .. })),
        "the 5 minute rule is a real rule"
    );
    let updated = updated_rows(&messages);
    assert_eq!(updated.len(), 1, "{messages:?}");
    assert!(updated[0].deferred);
    assert_eq!(updated[0].auto_answer, None, "a person chose this");
    assert_eq!(updated[0].action.as_deref(), Some("deny"));
    assert!(updated[0].matched_rule.is_some());
    assert_eq!(updated[0].answer_deadline_ms, None);
}

#[tokio::test(start_paused = true)]
async fn decide_later_without_a_bindable_program_gives_no_answer_now() {
    let (svc, cache, mut rx) = service();
    subscribe_with(&svc, "allow").await;
    let _gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, "Kernel connection");
    let pending = inserted(&mut rx).await;

    let outcome = decide_later(
        &cache,
        &svc.daemon_config_handle(),
        &svc.broadcast,
        &pending.id,
    )
    .await
    .unwrap();
    assert_eq!(outcome, DecidedLater::DefaultAction);

    let status = tokio::time::timeout(Duration::from_secs(1), ask)
        .await
        .expect("answered at once, not at the timeout")
        .unwrap()
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(status.message(), "no answer");
    let messages = drain(&mut rx);
    assert_nothing_saved(&svc, &messages);
    let updated = updated_rows(&messages);
    assert_eq!(updated.len(), 1, "{messages:?}");
    assert!(updated[0].deferred);
    assert_eq!(updated[0].auto_answer, None);
    assert_eq!(updated[0].action.as_deref(), Some("allow"));
    assert_eq!(updated[0].matched_rule, None);
}

#[tokio::test]
async fn decide_later_for_a_row_that_is_not_waiting_changes_nothing() {
    let (svc, cache, mut rx) = service();
    let result = decide_later(&cache, &svc.daemon_config_handle(), &svc.broadcast, "gone").await;
    assert!(result.is_err());
    assert!(drain(&mut rx).is_empty());
}
