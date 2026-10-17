use super::*;

#[tokio::test]
async fn ask_rule_auto_allows_immediately_when_filtering_paused() {
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus, filter_pause);
    // A pause only applies while a GUI is authenticated (see
    // `paused_bridge_without_an_authenticated_gui_defers_to_the_daemon`).
    let _gui_session = svc.client_presence().authenticated_session();

    // No spawn/wait needed: paused ask_rule never blocks on a oneshot.
    let rule = svc
        .ask_rule(Request::new(Connection {
            dst_host: "paused.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(rule.action, "allow");

    // No pending row was ever created.
    assert_eq!(cache.lock().await.pending_count(), 0);
    assert_eq!(cache.lock().await.len(), 1);

    let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("paused ask_rule did not broadcast the decided row")
        .expect("broadcast error");
    match broadcasted {
        ServerMessage::InsertConnectionRows { rows } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].action.as_deref(), Some("allow"));
        }
        other => panic!("expected InsertConnectionRows, got {other:?}"),
    }
}

#[tokio::test]
async fn paused_bridge_without_an_authenticated_gui_defers_to_the_daemon() {
    // Security review 2026-10-07 (issue #47): a pause is a GUI user's choice
    // and must not outlive every GUI session. With no authenticated GUI the
    // daemon's own default action applies, paused or not — otherwise a
    // paused system bridge would keep auto-allowing after logout.
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let filter_pause = Arc::new(FilterPause::new());
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(crate::tray_state::TrayStatePublisher::new()),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause,
    );

    let status = svc
        .ask_rule(Request::new(Connection {
            dst_host: "paused.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
        .expect_err("no authenticated GUI: the daemon must decide");
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(
        cache.lock().await.len(),
        0,
        "nothing auto-allowed or recorded"
    );
    assert!(
        rx.try_recv().is_err(),
        "no decided row may be broadcast for a deferred ask"
    );
}

#[tokio::test]
async fn ask_rule_prompts_normally_when_not_paused() {
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus, filter_pause);
    let _gui_session = svc.client_presence().authenticated_session();

    let ask_handle = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "normal.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if cache
            .lock()
            .await
            .resolve(
                &ask_row_id(1),
                Verdict::Allow,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .is_ok()
        {
            break;
        }
    }

    let rule = ask_handle.await.unwrap().unwrap().into_inner();
    assert_eq!(rule.action, "allow");
}

#[tokio::test]
async fn a_departed_guis_pause_does_not_auto_allow_for_the_next_gui() {
    // Security review F1: the last-loss clear runs in its own task. A GUI that
    // authenticates before it runs must still be prompted, not auto-allowed
    // under the departed GUI's pause.
    let (svc, cache, mut rx) = lifecycle_service();
    let presence = svc.client_presence();
    let gui_a = presence.authenticated_session();
    crate::client_presence::apply_pause_request(
        &presence,
        &svc.filter_pause,
        crate::filter_pause::PauseRequest::Pause(Duration::from_secs(300)),
        Some(presence.current_generation()),
        None,
    );
    drop(gui_a); // No clear task in this test: the gap stays open.
    let _gui_b = presence.authenticated_session();
    assert!(svc.filter_pause.is_active_now());

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "next-gui.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    let row_id = lifecycle_pending(&mut rx).await;
    assert_eq!(
        cache.lock().await.pending_count(),
        1,
        "GUI B was auto-allowed under GUI A's pause"
    );
    cache
        .lock()
        .await
        .resolve(
            &row_id,
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    assert_eq!(ask.await.unwrap().unwrap().into_inner().action, "deny");
}

#[tokio::test(start_paused = true)]
async fn an_expired_pause_prompts_before_its_expiry_tick_runs() {
    // Issue #47: `ask_rule` checks the deadline itself, so an expired pause
    // stops auto-allowing even before the expiry task clears it.
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause.clone(),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    tokio::time::advance(Duration::from_secs(301)).await;
    let mut tray_rx = tray_pub.subscribe();

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "after-pause.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), tray_rx.changed())
        .await
        .expect("no prompt: the expired pause auto-allowed")
        .unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));
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
    assert_eq!(ask.await.unwrap().unwrap().into_inner().action, "deny");
}
