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

// Drain the writer before inspecting durable outcomes. This also catches
// duplicate writes that could arrive after an early polling assertion.
fn prompt_outcomes(
    capture: Arc<Capture>,
    root: &tempfile::TempDir,
) -> Vec<(String, String, String, bool)> {
    Arc::try_unwrap(capture)
        .ok()
        .expect("sole capture")
        .shutdown();
    let state = root.path().canonicalize().unwrap().join("state");
    let location = locate(Mode::User, Some(&state)).unwrap();
    let conn = rusqlite::Connection::open(location.db_path()).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT prompt_outcome, answered_by, action, rule_hash IS NOT NULL
             FROM events ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[tokio::test]
async fn pause_records_one_outcome_for_waiting_and_new_prompts() {
    let (svc, mut rx) = service();
    let (capture, root) = live_capture();
    let svc = svc.with_history_capture(capture.clone());
    let presence = svc.client_presence();
    let _gui = presence.authenticated_session();
    let waiting = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(connection("waiting.example")))
                .await
        }
    });
    assert!(matches!(
        rx.recv().await.unwrap(),
        ServerMessage::InsertConnectionRows { .. }
    ));
    crate::client_presence::apply_pause_request(
        &presence,
        &svc.filter_pause,
        crate::filter_pause::PauseRequest::Pause(Duration::from_secs(300)),
        Some(presence.current_generation()),
        None,
    );
    assert_eq!(
        crate::pause_answers::answer_waiting(&svc.filter_pause, &svc.cache, &svc.broadcast).await,
        1
    );
    assert_eq!(waiting.await.unwrap().unwrap().into_inner().action, "allow");
    let new = svc
        .ask_rule(Request::new(connection("new.example")))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(new.action, "allow");
    drop(svc);
    let rows = prompt_outcomes(capture, &root);
    assert_eq!(rows.len(), 2);
    for (outcome, answered_by, action, hashed) in rows {
        assert_eq!(outcome, "replied");
        assert_eq!(answered_by, "filter-paused");
        assert_eq!(action, "allow");
        assert!(hashed);
    }
}

#[tokio::test(start_paused = true)]
async fn only_an_accepted_prompt_can_record_an_error_or_timeout() {
    let (svc, mut rx) = service();
    let (capture, root) = live_capture();
    let svc = svc.with_history_capture(capture.clone());
    assert!(svc
        .ask_rule(Request::new(connection("no-gui.example")))
        .await
        .is_err());
    let presence = svc.client_presence();
    let gui = presence.authenticated_session();
    let cache = svc.cache.lock().await;
    let mut ask = svc.ask_rule(Request::new(connection("lost-admission.example")));
    assert!(
        std::future::poll_fn(|cx| { std::task::Poll::Ready(ask.as_mut().poll(cx).is_pending()) })
            .await
    );
    drop(gui);
    drop(cache);
    assert_eq!(
        ask.await.unwrap_err().message(),
        "authenticated GUI session lost before admission"
    );
    let _gui = presence.authenticated_session();
    svc.daemon_config_handle()
        .set(crate::daemon_config::DaemonConfigView {
            default_action: Some("deny".into()),
            ..Default::default()
        });
    assert_eq!(
        svc.ask_rule(Request::new(connection("timeout.example")))
            .await
            .unwrap_err()
            .message(),
        "no answer"
    );
    while rx.try_recv().is_ok() {}
    let deferred = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                process_path: String::new(),
                ..connection("deferred.example")
            }))
            .await
        }
    });
    let ServerMessage::InsertConnectionRows { rows } = rx.recv().await.unwrap() else {
        panic!("expected a pending row");
    };
    assert_eq!(
        crate::deferred_answers::decide_later(
            &svc.cache,
            &svc.daemon_config,
            &svc.broadcast,
            &rows[0].id,
        )
        .await
        .unwrap(),
        crate::deferred_answers::DecidedLater::DefaultAction
    );
    assert_eq!(deferred.await.unwrap().unwrap_err().message(), "no answer");
    drop(svc);
    let rows = prompt_outcomes(capture, &root);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0],
        (
            "error".into(),
            "unknown".into(),
            "pending-unanswered".into(),
            false
        )
    );
    assert_eq!(
        rows[1],
        (
            "timeout".into(),
            "no-answer".into(),
            "default-deny".into(),
            false
        )
    );
    assert_eq!(
        rows[2],
        (
            "error".into(),
            "unknown".into(),
            "default-deny".into(),
            false
        )
    );
}

#[tokio::test(start_paused = true)]
async fn gui_loss_wins_if_the_timeout_has_not_settled() {
    let (svc, mut rx) = service();
    let (capture, root) = live_capture();
    let svc = svc.with_history_capture(capture.clone());
    let gui = svc.client_presence().authenticated_session();
    let ask = tokio::spawn({
        let svc = svc.clone();
        async move { svc.ask_rule(Request::new(connection("race.example"))).await }
    });
    assert!(matches!(
        rx.recv().await.unwrap(),
        ServerMessage::InsertConnectionRows { .. }
    ));
    let cache = svc.cache.lock().await;
    tokio::time::advance(crate::deferred_answers::ANSWER_TIMEOUT).await;
    drop(gui);
    drop(cache);
    assert_eq!(
        ask.await.unwrap().unwrap_err().message(),
        "last authenticated GUI session disconnected"
    );
    drop(svc);
    let rows = prompt_outcomes(capture, &root);
    assert_eq!(
        rows,
        [(
            "gui-lost".into(),
            "unknown".into(),
            "pending-unanswered".into(),
            false
        )]
    );
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
