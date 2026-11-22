//! PR 5, the hook in `ping`: history capture runs only after the connection
//! cache lock is dropped, never changes what a ping does or replies, and with
//! history off is one `None` check.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::history::capture::Capture;
use crate::history::durable::tests::engine_at;
use crate::history::pins::PinRegistry;
use crate::history::test_support::{state_dir, tempdir};
use crate::history::{enable, locate, Consent, Mode};
use snitchwatch_proto::protocol::{Event, Operator, Statistics};

fn service() -> (UiService, broadcast::Receiver<ServerMessage>) {
    let (tx, rx) = broadcast::channel(64);
    let svc = UiService::new(
        Arc::new(Mutex::new(ConnectionCache::new(64))),
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    (svc, rx)
}

/// A real, enabled capture in a tempdir.
fn live_capture() -> (Arc<Capture>, tempfile::TempDir) {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let engine = engine_at(&location);
    let config = crate::history::capture::CaptureConfig::for_budget(engine.budget());
    let capture = Capture::spawn(engine, Arc::new(PinRegistry::new()), config).unwrap();
    (Arc::new(capture), root)
}

fn connection(host: &str) -> Connection {
    Connection {
        protocol: "tcp".into(),
        dst_host: host.into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: "/usr/bin/curl".into(),
        ..Default::default()
    }
}

fn matched(host: &str, rule_name: &str, duration: &str) -> Event {
    Event {
        connection: Some(connection(host)),
        rule: Some(Rule {
            name: rule_name.into(),
            action: "allow".into(),
            duration: duration.into(),
            enabled: true,
            operator: Some(Operator {
                r#type: "simple".into(),
                operand: "dest.host".into(),
                data: host.into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        unixnano: 1_700_000_000_000_000_000,
        ..Default::default()
    }
}

async fn ping(svc: &UiService, events: Vec<Event>) -> PingReply {
    svc.ping(Request::new(PingRequest {
        id: 7,
        stats: Some(Statistics {
            events,
            uptime: 5,
            rule_hits: 2,
            ..Default::default()
        }),
    }))
    .await
    .unwrap()
    .into_inner()
}

/// The rows a ping produced, with the ids (a process-wide counter) blanked.
fn rows_without_ids(msg: ServerMessage) -> Vec<ConnectionRow> {
    match msg {
        ServerMessage::InsertConnectionRows { rows } => rows
            .into_iter()
            .map(|mut r| {
                r.id = String::new();
                r
            })
            .collect(),
        other => panic!("expected rows, got {other:?}"),
    }
}

fn first_rows(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ConnectionRow> {
    loop {
        match rx.try_recv().expect("a row broadcast") {
            msg @ ServerMessage::InsertConnectionRows { .. } => return rows_without_ids(msg),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn a_service_has_no_capture_unless_one_is_given() {
    let (svc, _rx) = service();
    assert!(svc.history_capture.is_none());
}

#[tokio::test]
async fn a_ping_replies_and_broadcasts_the_same_with_history_off_and_on() {
    let events = || {
        vec![
            matched("example.com", "allow-example", "always"),
            matched("example.org", "allow-example", "always"),
        ]
    };
    let (off, mut off_rx) = service();
    let (on, mut on_rx) = service();
    let (capture, _root) = live_capture();
    let on = on.with_history_capture(capture.clone());

    let off_reply = ping(&off, events()).await;
    let on_reply = ping(&on, events()).await;

    assert_eq!(off_reply, on_reply);
    let off_rows = first_rows(&mut off_rx);
    assert_eq!(off_rows.len(), 2);
    assert_eq!(off_rows, first_rows(&mut on_rx));
    let cached = |svc: &UiService| {
        let cache = svc.cache.clone();
        async move {
            let cache = cache.lock().await;
            cache
                .rows()
                .iter()
                .cloned()
                .map(|mut r| {
                    r.id = String::new();
                    r
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(cached(&off).await, cached(&on).await);
    assert_eq!(cached(&on).await.len(), 2);
    // And the one with capture on did capture them.
    assert_eq!(capture.stats().seen, 2);
}

#[tokio::test]
async fn capture_runs_after_the_cache_and_rules_locks_are_released() {
    let (svc, _rx) = service();
    let (capture, _root) = live_capture();
    let cache = svc.cache.clone();
    let rules = svc.rules_handle();
    let calls = Arc::new(AtomicUsize::new(0));
    let cache_was_free = Arc::new(AtomicBool::new(false));
    let rules_were_free = Arc::new(AtomicBool::new(false));
    let (c, cf, rf) = (
        calls.clone(),
        cache_was_free.clone(),
        rules_were_free.clone(),
    );
    capture.set_probe_for_test(Box::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        cf.store(cache.try_lock().is_ok(), Ordering::SeqCst);
        rf.store(rules.try_lock().is_ok(), Ordering::SeqCst);
    }));
    let svc = svc.with_history_capture(capture);
    ping(
        &svc,
        vec![matched("example.com", "allow-example", "always")],
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        cache_was_free.load(Ordering::SeqCst),
        "capture ran under the cache lock"
    );
    assert!(
        rules_were_free.load(Ordering::SeqCst),
        "capture ran under the rules lock"
    );
}

#[tokio::test]
async fn only_events_that_became_rows_are_captured() {
    let (svc, _rx) = service();
    let (capture, _root) = live_capture();
    let svc = svc.with_history_capture(capture.clone());
    // A known (empty) rule list: a once answer named like the bridge's own,
    // with no such rule listed, is the bridge's own prompt answer.
    svc.rules_handle()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .replace_all(vec![]);
    // The bridge's own once answer is not listed twice, so not captured twice.
    let once = format!(
        "{}example",
        crate::translator::verdict::answer_name_prefix(Verdict::Allow)
    );
    let mut no_rule = matched("example.net", "x", "always");
    no_rule.rule = None;
    ping(
        &svc,
        vec![
            matched("example.com", &once, "once"),
            no_rule,
            matched("example.org", "kept", "always"),
        ],
    )
    .await;
    assert_eq!(capture.stats().seen, 1);
}

#[tokio::test]
async fn a_ping_with_no_events_never_calls_capture() {
    let (svc, _rx) = service();
    let (capture, _root) = live_capture();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    capture.set_probe_for_test(Box::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    }));
    let svc = svc.with_history_capture(capture);
    ping(&svc, vec![]).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
