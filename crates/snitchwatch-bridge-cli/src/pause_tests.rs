use super::*;

#[tokio::test]
async fn pause_request_without_an_authenticated_gui_is_ignored() {
    // A pause still queued when its GUI disconnected arrives with no
    // session; it must not re-arm the pause for the next GUI (#47).
    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let mut bridge = run(cfg).await.expect("run failed");

    bridge
        .inbound_tx
        .send(set_filtering_paused(true, Some(1800)))
        .await
        .expect("inbound channel closed");
    // The pump always publishes a tray state for a pause request, so
    // wait for it: an applied pause would show FilterOff.
    tokio::time::timeout(Duration::from_secs(5), bridge.tray_rx.changed())
        .await
        .expect("pump did not handle the pause request")
        .unwrap();
    assert_ne!(*bridge.tray_rx.borrow(), TrayState::FilterOff);
    // A GUI arriving afterwards must not inherit a pause. (A GUI that
    // registers before the pump runs is covered by the sender-generation
    // stamp; see `client_presence`'s tests.)
    let _gui = bridge.client_presence.authenticated_session();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), bridge.tray_rx.changed())
            .await
            .is_err(),
        "nothing should re-publish FilterOff"
    );
    assert_ne!(*bridge.tray_rx.borrow(), TrayState::FilterOff);

    bridge.shutdown();
}

fn set_filtering_paused(paused: bool, duration_secs: Option<u64>) -> ClientMessage {
    ClientMessage::SetFilteringPaused {
        paused,
        duration_secs,
        sender_generation: None,
        sender_uid: None,
    }
}

fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn test_bridge(dir: &tempfile::TempDir) -> RunningBridge {
    run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    })
    .await
    .expect("run failed")
}

/// Wait for the next `FilterPauseState` broadcast, skipping unrelated
/// messages and counting `FilterPauseExpired` notices on the way.
async fn next_pause_state(
    rx: &mut broadcast::Receiver<ServerMessage>,
    expiry_notices: &mut usize,
) -> (bool, Option<u64>) {
    loop {
        match rx.recv().await.expect("broadcast channel closed") {
            ServerMessage::FilterPauseState {
                paused,
                expires_at_unix_ms,
            } => return (paused, expires_at_unix_ms),
            ServerMessage::Notice {
                notice: Notice::FilterPauseExpired,
            } => *expiry_notices += 1,
            _ => {}
        }
    }
}

#[tokio::test]
async fn set_filtering_paused_toggles_tray_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = test_bridge(&dir).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut expiry_notices = 0;
    // A pause only takes effect while a GUI is authenticated (#47).
    let _gui = bridge.client_presence.authenticated_session();

    let before = unix_ms_now();
    bridge
        .inbound_tx
        .send(set_filtering_paused(true, Some(1800)))
        .await
        .expect("inbound channel closed");
    bridge.tray_rx.changed().await.unwrap();
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::FilterOff);
    let (paused, expires_at) = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("pause state was not broadcast");
    assert!(paused);
    let expires_at = expires_at.expect("a pause carries its end time");
    assert!(
        (before + 1_800_000..=unix_ms_now() + 1_800_000).contains(&expires_at),
        "a 30-minute pause must end 30 minutes from now, got {expires_at}"
    );

    bridge
        .inbound_tx
        .send(set_filtering_paused(false, None))
        .await
        .expect("inbound channel closed");
    bridge.tray_rx.changed().await.unwrap();
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);
    let resumed = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("resume was not broadcast");
    assert_eq!(resumed, (false, None));
    assert_eq!(expiry_notices, 0);

    bridge.shutdown();
}

#[tokio::test]
async fn a_snapshot_mid_pause_reports_the_pause_and_its_end() {
    // A GUI that connects mid-pause learns the state and the end time.
    let dir = tempfile::tempdir().unwrap();
    let bridge = test_bridge(&dir).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut expiry_notices = 0;
    let _gui = bridge.client_presence.authenticated_session();
    bridge
        .inbound_tx
        .send(set_filtering_paused(true, Some(1800)))
        .await
        .expect("inbound channel closed");
    let (_, announced_end) = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("pause state was not broadcast");
    let announced_end = announced_end.expect("a pause carries its end time");

    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .expect("inbound channel closed");
    let (paused, snapshot_end) = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("the snapshot carried no pause state");
    assert!(paused);
    let snapshot_end = snapshot_end.expect("a pause carries its end time");
    assert!(
        snapshot_end.abs_diff(announced_end) < 1_000,
        "snapshot end {snapshot_end} drifted from {announced_end}"
    );
    assert!(
        snapshot_end > unix_ms_now() + 1_790_000,
        "most of the 30 minutes remain"
    );
    bridge.shutdown();
}

#[tokio::test]
async fn legacy_pause_without_a_duration_expires_after_five_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = test_bridge(&dir).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut expiry_notices = 0;
    let _gui = bridge.client_presence.authenticated_session();

    // The bridge started on real time; from here on the test drives the
    // clock (auto-advancing whenever the runtime is idle).
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    bridge
        .inbound_tx
        .send(set_filtering_paused(true, None))
        .await
        .expect("inbound channel closed");
    let (paused, _) = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("pause state was not broadcast");
    assert!(paused, "an old client's pause must still work");
    assert_eq!(*bridge.tray_rx.borrow_and_update(), TrayState::FilterOff);

    let (paused, expires_at) = tokio::time::timeout(
        Duration::from_secs(600),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("the pause never expired");
    let elapsed = start.elapsed();
    assert_eq!((paused, expires_at), (false, None));
    assert!(
        (Duration::from_secs(300)..=Duration::from_secs(301)).contains(&elapsed),
        "legacy pause ended after {elapsed:?}, not 300 s"
    );
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);

    // The expiry notice is relayed separately; collect it, and make sure
    // there is exactly one.
    tokio::time::sleep(Duration::from_secs(10)).await;
    while let Ok(msg) = rx.try_recv() {
        if let ServerMessage::Notice {
            notice: Notice::FilterPauseExpired,
        } = msg
        {
            expiry_notices += 1;
        }
    }
    assert_eq!(expiry_notices, 1);
    bridge.shutdown();
}

#[tokio::test]
async fn losing_the_last_gui_clears_the_pause_without_an_expiry_notice() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = test_bridge(&dir).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut expiry_notices = 0;
    let gui = bridge.client_presence.authenticated_session();

    bridge
        .inbound_tx
        .send(set_filtering_paused(true, Some(3600)))
        .await
        .expect("inbound channel closed");
    let (paused, _) = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("pause state was not broadcast");
    assert!(paused);

    drop(gui);
    let cleared = tokio::time::timeout(
        Duration::from_secs(5),
        next_pause_state(&mut rx, &mut expiry_notices),
    )
    .await
    .expect("clearing the pause was not broadcast");
    assert_eq!(cleared, (false, None));
    assert_eq!(*bridge.tray_rx.borrow_and_update(), TrayState::Idle);

    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(msg) = rx.try_recv() {
        if let ServerMessage::Notice {
            notice: Notice::FilterPauseExpired,
        } = msg
        {
            expiry_notices += 1;
        }
    }
    assert_eq!(expiry_notices, 0, "no GUI is left to show an expiry");
    bridge.shutdown();
}
