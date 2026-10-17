use super::*;

#[tokio::test(start_paused = true)]
async fn ask_rule_deny_publishes_recent_block_then_reverts_to_idle() {
    use crate::translator::connection::ask_row_id;
    use crate::ws_messages::{VerdictDuration, VerdictScope};

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        notice_bus,
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();

    let mut tray_rx = tray_pub.subscribe();

    let svc_for_ask = svc.clone();
    let ask_handle = tokio::spawn(async move {
        svc_for_ask
            .ask_rule(Request::new(Connection {
                protocol: "tcp".into(),
                dst_host: "tracker.example.com".into(),
                dst_ip: "1.2.3.4".into(),
                dst_port: 80,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
    });

    tray_rx.changed().await.unwrap();
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
    ask_handle.await.unwrap().unwrap();

    tray_rx.changed().await.unwrap();
    match &*tray_rx.borrow() {
        TrayState::RecentBlock { what, .. } => {
            assert!(what.contains("tracker.example.com"), "unexpected: {what}")
        }
        other => panic!("expected RecentBlock, got {other:?}"),
    }

    tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);
}

#[tokio::test(start_paused = true)]
async fn second_deny_within_ttl_supersedes_first_blocks_revert_timer() {
    use crate::translator::connection::ask_row_id;
    use crate::ws_messages::{VerdictDuration, VerdictScope};

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        notice_bus,
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let mut tray_rx = tray_pub.subscribe();

    // First block.
    let svc1 = svc.clone();
    let ask1 = tokio::spawn(async move {
        svc1.ask_rule(Request::new(Connection {
            dst_host: "first.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
    });
    tray_rx.changed().await.unwrap();
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
    ask1.await.unwrap().unwrap();
    tray_rx.changed().await.unwrap();
    assert!(matches!(&*tray_rx.borrow(), TrayState::RecentBlock { .. }));

    // Halfway through the first block's TTL, a second block supersedes it.
    tokio::time::advance(RECENT_BLOCK_TTL / 2).await;
    let svc2 = svc.clone();
    let ask2 = tokio::spawn(async move {
        svc2.ask_rule(Request::new(Connection {
            dst_host: "second.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
    });
    tray_rx.changed().await.unwrap(); // Pending(1) for the second ask
    cache
        .lock()
        .await
        .resolve(
            &ask_row_id(2),
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    ask2.await.unwrap().unwrap();
    tray_rx.changed().await.unwrap();
    match &*tray_rx.borrow() {
        TrayState::RecentBlock { what, .. } => assert!(what.contains("second.example.com")),
        other => panic!("expected RecentBlock(second), got {other:?}"),
    }

    // When the FIRST block's original TTL would have elapsed, its timer
    // must be a no-op — the tray should still show the second block.
    tokio::time::advance(RECENT_BLOCK_TTL / 2 + Duration::from_millis(50)).await;
    assert!(
        matches!(&*tray_rx.borrow(), TrayState::RecentBlock { what, .. } if what.contains("second.example.com")),
        "first block's timer must not have reverted the tray"
    );

    // Only once the SECOND block's own TTL elapses does it revert.
    tokio::time::advance(RECENT_BLOCK_TTL).await;
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);
}

#[tokio::test(start_paused = true)]
async fn recent_block_reverts_to_filter_off_while_paused() {
    // Issue #47: a prompt raised before the pause and denied mid-pause shows
    // RecentBlock, and its revert must land on FilterOff, not Idle.
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let filter_pause = Arc::new(FilterPause::new());
    let cache = Arc::new(Mutex::new(
        ConnectionCache::with_tray_publisher(64, tray_pub.clone())
            .with_filter_pause(filter_pause.clone()),
    ));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause.clone(),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let mut tray_rx = tray_pub.subscribe();

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "tracker.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    cache.lock().await.resync_tray_state();
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
    assert!(matches!(
        &*tray_rx.borrow_and_update(),
        TrayState::RecentBlock { .. }
    ));

    tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::FilterOff);
}

#[tokio::test(start_paused = true)]
async fn a_deny_while_the_daemon_is_down_does_not_cover_the_daemon_down_tray() {
    // Issue #58 follow-up: a Deny that lands in a hung-daemon window used to
    // put "Blocked: X" over DaemonDown for the whole TTL.
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        Arc::new(crate::notice::NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let mut tray_rx = tray_pub.subscribe();

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "tracker.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

    // The watchdog marks the daemon down while the prompt is open.
    {
        let mut cache = cache.lock().await;
        cache.set_daemon_down(true);
        tray_pub.set(cache.tray_state());
    }
    assert_eq!(*tray_rx.borrow_and_update(), TrayState::DaemonDown);

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
    assert_eq!(
        *tray_rx.borrow_and_update(),
        TrayState::DaemonDown,
        "the block overlay must not cover DaemonDown"
    );

    // Nothing is left to revert, and recovery lands on the derived state.
    tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
    assert_eq!(*tray_rx.borrow(), TrayState::DaemonDown);
    {
        let mut cache = cache.lock().await;
        cache.set_daemon_down(false);
        tray_pub.set(cache.tray_state());
    }
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);
}

#[test]
fn process_bound_verdict_rule_survives_the_wire_round_trip() {
    // Issue #44: toggling a rule in the GUI sends its wire shape back through
    // `rule_from_wire` as a CHANGE_RULE, which the daemon applies wholesale —
    // so the process binding must come back intact, case sensitivity included.
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: "/usr/bin/curl".into(),
        ..Default::default()
    };
    let rule = crate::translator::verdict::verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
        &conn,
        0,
    )
    .expect("absolute process path");
    let back = crate::rule_wire::rule_from_wire(&crate::rule_wire::rule_to_wire(&rule)).unwrap();
    assert_eq!(back.name, rule.name);
    let op = back.operator.unwrap();
    assert_eq!(op.r#type, "list");
    assert_eq!(op.list.len(), 2, "{op:?}");
    let (process, host) = (&op.list[0], &op.list[1]);
    assert_eq!(process.operand, "process.path");
    assert_eq!(process.data, "/usr/bin/curl");
    assert!(process.sensitive);
    assert_eq!(host.operand, "dest.host");
    assert_eq!(host.data, "example.com");
}
