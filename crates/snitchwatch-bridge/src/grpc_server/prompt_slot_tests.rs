//! Prompt-slot visibility through `ask_rule` and `ping` (plan
//! `2026-10-08-prompt-slot-ux.md`, part A): a prompt holds the slot once its
//! row is announced, every exit releases it, and the daemon's counters give
//! "at least N defaulted" from a baseline taken after the hold.

use super::*;
use crate::cache::connections::Verdict;
use crate::notice::Notice;
use crate::translator::connection::ask_row_id;
use crate::ws_messages::{VerdictDuration, VerdictScope};
use snitchwatch_proto::protocol::{NotificationReply, NotificationReplyCode, Statistics};
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
    (svc, cache, rx, notices)
}

fn connection(process_path: &str, dst_host: &str) -> Connection {
    Connection {
        protocol: "tcp".into(),
        dst_host: dst_host.into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: process_path.into(),
        ..Default::default()
    }
}

/// (holder row id, holder `what`, holders, defaulted) of a `PromptSlot`.
type Slot = (Option<String>, Option<String>, u32, Option<u64>);

fn as_slot(message: &ServerMessage) -> Option<Slot> {
    match message {
        ServerMessage::PromptSlot {
            holder,
            holders,
            defaulted_at_least,
        } => Some((
            holder.as_ref().map(|h| h.row_id.clone()),
            holder.as_ref().map(|h| h.what.clone()),
            *holders,
            *defaulted_at_least,
        )),
        _ => None,
    }
}

/// The next broadcast message, whatever it is.
async fn next(rx: &mut broadcast::Receiver<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("no broadcast")
        .expect("broadcast closed")
}

/// The next `PromptSlot` message, skipping others.
async fn next_slot(rx: &mut broadcast::Receiver<ServerMessage>) -> Slot {
    loop {
        if let Some(slot) = as_slot(&next(rx).await) {
            return slot;
        }
    }
}

async fn ping(svc: &UiService, rule_misses: u64, uptime: u64) {
    svc.ping(Request::new(PingRequest {
        id: 1,
        stats: Some(Statistics {
            rule_misses,
            uptime,
            ..Default::default()
        }),
    }))
    .await
    .unwrap();
}

fn spawn_ask(
    svc: &UiService,
    conn: Connection,
) -> tokio::task::JoinHandle<Result<Response<Rule>, Status>> {
    let asking = svc.clone();
    tokio::spawn(async move { asking.ask_rule(Request::new(conn)).await })
}

#[tokio::test]
async fn a_prompt_holds_the_slot_after_its_row_and_a_verdict_releases_it() {
    let (svc, cache, mut rx, _notices) = service();
    let _gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, connection("/usr/bin/curl", "example.com"));

    assert!(
        matches!(
            next(&mut rx).await,
            ServerMessage::InsertConnectionRows { .. }
        ),
        "the row comes first, so a client can act on the holder"
    );
    assert_eq!(
        as_slot(&next(&mut rx).await),
        Some((
            Some(ask_row_id(1)),
            Some("curl → example.com".to_string()),
            1,
            None
        ))
    );

    cache
        .lock()
        .await
        .resolve(
            &ask_row_id(1),
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    ask.await.unwrap().unwrap();
    assert_eq!(next_slot(&mut rx).await, (None, None, 0, None));
}

#[tokio::test]
async fn losing_the_gui_releases_the_slot() {
    let (svc, _cache, mut rx, _notices) = service();
    let gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, connection("/usr/bin/curl", "example.com"));
    assert_eq!(next_slot(&mut rx).await.2, 1);
    drop(gui);
    assert!(ask.await.unwrap().is_err());
    assert_eq!(next_slot(&mut rx).await, (None, None, 0, None));
}

#[tokio::test]
async fn a_cancelled_ask_releases_the_slot() {
    let (svc, _cache, mut rx, _notices) = service();
    let _gui = svc.client_presence().authenticated_session();
    let ask = spawn_ask(&svc, connection("/usr/bin/curl", "example.com"));
    assert_eq!(next_slot(&mut rx).await.2, 1);
    // The daemon's deadline or a dropped connection drops the handler future.
    ask.abort();
    assert_eq!(next_slot(&mut rx).await, (None, None, 0, None));
}

#[tokio::test]
async fn pings_count_defaulted_connections_and_the_release_sums_them_up() {
    let (svc, cache, mut rx, mut notices) = service();
    let _gui = svc.client_presence().authenticated_session();
    // A reading from before the prompt never becomes its baseline.
    ping(&svc, 50, 400).await;
    let ask = spawn_ask(&svc, connection("/usr/bin/curl", "example.com"));
    assert_eq!(next_slot(&mut rx).await.3, None);

    ping(&svc, 100, 500).await;
    ping(&svc, 105, 510).await;
    assert_eq!(next_slot(&mut rx).await.3, Some(5));

    cache
        .lock()
        .await
        .resolve(
            &ask_row_id(1),
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    ask.await.unwrap().unwrap();
    let summaries: Vec<Notice> = std::iter::from_fn(|| notices.try_recv().ok())
        .filter(|n| matches!(n, Notice::PromptSlotSummary { .. }))
        .collect();
    assert_eq!(
        summaries,
        vec![Notice::PromptSlotSummary {
            row_id: 1,
            count: 5
        }]
    );
}

#[tokio::test]
async fn a_new_rule_snapshot_mid_hold_starts_a_fresh_baseline() {
    let (svc, _cache, mut rx, _notices) = service();
    let _gui = svc.client_presence().authenticated_session();
    let _ask = spawn_ask(&svc, connection("/usr/bin/curl", "example.com"));
    assert_eq!(next_slot(&mut rx).await.2, 1);
    ping(&svc, 100, 500).await;

    // A daemon connection subscribes and says HELLO: a new snapshot commits.
    svc.subscribe(Request::new(ClientConfig::default()))
        .await
        .unwrap();
    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);
    commands.on_reply(
        stream.id(),
        &NotificationReply {
            id: 0,
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        },
    );

    ping(&svc, 110, 510).await;
    ping(&svc, 113, 520).await;
    assert_eq!(
        next_slot(&mut rx).await.3,
        Some(3),
        "counted from the reading after the new snapshot, not 13"
    );
}

#[tokio::test]
async fn a_paused_ask_never_holds_the_slot() {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel(64);
    let pause = Arc::new(FilterPause::new());
    pause.pause(Duration::from_secs(300), 0).unwrap();
    let svc = UiService::new(
        cache,
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        pause,
    );
    let _gui = svc.client_presence().authenticated_session();
    svc.ask_rule(Request::new(connection("/usr/bin/curl", "example.com")))
        .await
        .unwrap();
    while let Ok(message) = rx.try_recv() {
        assert!(as_slot(&message).is_none(), "{message:?}");
    }
}

#[tokio::test]
async fn the_holder_names_the_program_and_host_as_plain_text() {
    let (svc, _cache, mut rx, _notices) = service();
    let _gui = svc.client_presence().authenticated_session();
    let _ask = spawn_ask(
        &svc,
        connection("/tmp/<i>evil\u{202e}", "x&y.example\u{1b}[31m"),
    );
    assert_eq!(
        next_slot(&mut rx).await.1.as_deref(),
        Some("<i>evil → x&y.example[31m"),
        "hazards removed, nothing escaped: clients show it as plain text"
    );
}
