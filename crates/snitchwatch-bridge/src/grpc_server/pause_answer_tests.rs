//! Issue #78 through `ask_rule`: a pause answers the prompts already waiting
//! Allow once (never a saved rule) and labels their rows, an Ask that reaches
//! the cache as the pause lands is allowed rather than left waiting, and a
//! prompt admitted under another GUI session's generation is not answered.

use super::*;
use crate::cache::connections::Verdict;
use crate::cache::rules::RulesCache;
use crate::filter_pause::PauseRequest;
use crate::notice::Notice;
use crate::pause_answers::{allow_waiting, answer_waiting};
use crate::ws_messages::{AutoAnswer, ConnectionRow, VerdictDuration, VerdictScope};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};

fn service() -> (
    UiService,
    Arc<Mutex<ConnectionCache>>,
    broadcast::Receiver<ServerMessage>,
    broadcast::Receiver<Notice>,
) {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, rx) = broadcast::channel(64);
    let notice_bus = Arc::new(NoticeBus::new());
    let notices = notice_bus.subscribe();
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(TrayStatePublisher::new()),
        notice_bus,
        Arc::new(FilterPause::new()),
    );
    // Synced and empty, so a remembered verdict would show up in it.
    svc.rules_handle().lock().unwrap().replace_all(Vec::new());
    (svc, cache, rx, notices)
}

fn spawn_ask(
    svc: &UiService,
    dst_host: &str,
) -> tokio::task::JoinHandle<Result<Response<Rule>, Status>> {
    let asking = svc.clone();
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: dst_host.into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: "/usr/bin/curl".into(),
        ..Default::default()
    };
    tokio::spawn(async move { asking.ask_rule(Request::new(conn)).await })
}

/// A tray "Pause for 5 minutes" from the GUI session of `generation`.
fn pause_from(svc: &UiService, generation: u64) {
    crate::client_presence::apply_pause_request(
        &svc.client_presence(),
        &svc.filter_pause,
        PauseRequest::Pause(Duration::from_secs(300)),
        Some(generation),
        None,
    );
}

/// The row of the next `InsertConnectionRows`.
async fn inserted(rx: &mut broadcast::Receiver<ServerMessage>) -> ConnectionRow {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerMessage::InsertConnectionRows { rows } = rx.recv().await.unwrap() {
                return rows[0].clone();
            }
        }
    })
    .await
    .expect("no row inserted")
}

/// Every message broadcast and not yet read.
fn drain(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

async fn reply(ask: tokio::task::JoinHandle<Result<Response<Rule>, Status>>) -> Rule {
    tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .expect("the Ask was left waiting")
        .unwrap()
        .unwrap()
        .into_inner()
}

fn assert_allowed_once_and_not_saved(svc: &UiService, rule: &Rule, messages: &[ServerMessage]) {
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(
        !messages
            .iter()
            .any(|m| matches!(m, ServerMessage::UpdateRules { .. })),
        "a once answer must not be saved as a rule: {messages:?}"
    );
    assert_eq!(
        *svc.rules_handle().lock().unwrap(),
        RulesCache::Synced(Default::default())
    );
}

/// An Ask decided on arrival: one labelled, allowed row and no prompt.
fn assert_allowed_on_arrival(
    messages: &[ServerMessage],
    notices: &mut broadcast::Receiver<Notice>,
) {
    let rows: Vec<&ConnectionRow> = messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::InsertConnectionRows { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(rows.len(), 1, "{messages:?}");
    assert_eq!(rows[0].action.as_deref(), Some("allow"));
    assert_eq!(rows[0].auto_answer, Some(AutoAnswer::FilterPaused));
    assert!(
        !std::iter::from_fn(|| notices.try_recv().ok())
            .any(|n| matches!(n, Notice::Pending { .. })),
        "an Ask the pause decides is never announced as waiting"
    );
}

#[tokio::test]
async fn a_pause_answers_a_waiting_prompt_allow_once_and_labels_its_row() {
    let (svc, cache, mut rx, _notices) = service();
    let presence = svc.client_presence();
    let _gui = presence.authenticated_session();
    let ask = spawn_ask(&svc, "waiting.example.com");
    let pending = inserted(&mut rx).await;
    assert_eq!(pending.action, None);

    pause_from(&svc, presence.current_generation());
    assert_eq!(
        answer_waiting(&svc.filter_pause, &cache, &svc.broadcast).await,
        1
    );

    let rule = reply(ask).await;
    let messages = drain(&mut rx);
    assert_allowed_once_and_not_saved(&svc, &rule, &messages);
    let updated: Vec<&ConnectionRow> = messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::UpdateConnectionRows { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(updated.len(), 1, "{messages:?}");
    assert_eq!(updated[0].id, pending.id);
    assert_eq!(updated[0].action.as_deref(), Some("allow"));
    assert_eq!(updated[0].auto_answer, Some(AutoAnswer::FilterPaused));
    let cache = cache.lock().await;
    assert_eq!(cache.pending_count(), 0);
    assert_eq!(cache.rows()[0].auto_answer, Some(AutoAnswer::FilterPaused));
}

#[tokio::test]
async fn a_pause_left_by_a_departed_gui_session_answers_nothing() {
    let (svc, cache, mut rx, _notices) = service();
    let presence = svc.client_presence();
    let gui_a = presence.authenticated_session();
    pause_from(&svc, presence.current_generation());
    drop(gui_a); // No clear task in this test: GUI A's pause stays set.
    let _gui_b = presence.authenticated_session();
    assert!(svc.filter_pause.is_active_now());
    let ask = spawn_ask(&svc, "next-gui.example.com");
    let pending = inserted(&mut rx).await;
    assert_eq!(pending.action, None);

    assert_eq!(
        answer_waiting(&svc.filter_pause, &cache, &svc.broadcast).await,
        0,
        "GUI A's pause answered GUI B's prompt"
    );
    assert_eq!(cache.lock().await.pending_count(), 1);
    cache
        .lock()
        .await
        .resolve(
            &pending.id,
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    assert_eq!(reply(ask).await.action, "deny");
}

#[tokio::test]
async fn an_ask_reaching_the_cache_as_the_pause_lands_is_allowed_not_left_waiting() {
    let (svc, cache, mut rx, mut notices) = service();
    let presence = svc.client_presence();
    let _gui = presence.authenticated_session();

    // The pause's scan holds the cache lock. The Ask arrives just before the
    // pause is set and waits for that lock.
    let mut scanning = cache.lock().await;
    let ask = spawn_ask(&svc, "racing.example.com");
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    pause_from(&svc, presence.current_generation());
    assert!(
        allow_waiting(&mut scanning, &svc.filter_pause).is_empty(),
        "nothing was waiting yet"
    );
    drop(scanning);

    let rule = reply(ask).await;
    let messages = drain(&mut rx);
    assert_allowed_once_and_not_saved(&svc, &rule, &messages);
    assert_allowed_on_arrival(&messages, &mut notices);
    assert_eq!(cache.lock().await.pending_count(), 0);
}

#[tokio::test]
async fn an_ask_during_the_pause_is_allowed_once_with_the_same_label() {
    let (svc, cache, mut rx, mut notices) = service();
    let presence = svc.client_presence();
    let _gui = presence.authenticated_session();
    pause_from(&svc, presence.current_generation());

    let rule = reply(spawn_ask(&svc, "during.example.com")).await;
    let messages = drain(&mut rx);
    assert_allowed_once_and_not_saved(&svc, &rule, &messages);
    assert_allowed_on_arrival(&messages, &mut notices);
    assert_eq!(
        cache.lock().await.rows()[0].auto_answer,
        Some(AutoAnswer::FilterPaused)
    );
}
