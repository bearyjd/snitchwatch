use super::*;

#[test]
fn system_and_custom_socket_token_paths_match_the_service_layout() {
    let legacy_dir = std::path::Path::new("/tmp/legacy-runtime/snitchwatch");
    assert_eq!(
        resolve_socket_path(None, false, legacy_dir),
        legacy_dir.join("bridge.sock")
    );
    assert_eq!(
        resolve_socket_path(None, true, legacy_dir),
        PathBuf::from(SYSTEM_SOCKET_PATH)
    );
    for system_mode in [false, true] {
        assert_eq!(
            resolve_socket_path(Some("/tmp/explicit.sock".into()), system_mode, legacy_dir),
            PathBuf::from("/tmp/explicit.sock")
        );
    }
    assert_eq!(
        resolve_token_path(std::path::Path::new(SYSTEM_SOCKET_PATH), None),
        PathBuf::from(SYSTEM_TOKEN_PATH)
    );
    assert_eq!(
        resolve_token_path(std::path::Path::new("/tmp/custom/bridge.sock"), None),
        PathBuf::from("/tmp/custom/token")
    );
    assert_eq!(
        resolve_token_path(std::path::Path::new("bridge.sock"), None),
        PathBuf::from("token")
    );
    assert_eq!(
        resolve_token_path(
            std::path::Path::new(SYSTEM_SOCKET_PATH),
            Some("/tmp/explicit-token".into())
        ),
        PathBuf::from("/tmp/explicit-token")
    );
}

/// Accept one client, check its token, acknowledge it and read its snapshot
/// request. Returns the connection, which closes when the caller drops it.
async fn accept_authenticated_snapshot(
    listener: &tokio::net::UnixListener,
    expected_token: &snitchwatch_bridge::auth::Token,
) -> tokio_tungstenite::WebSocketStream<tokio::net::UnixStream> {
    let (stream, _) = listener.accept().await.expect("client connects");
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .expect("WebSocket upgrade succeeds");
    let presented = ws
        .next()
        .await
        .expect("client sends its token")
        .expect("token frame is valid")
        .into_text()
        .expect("token is a text frame");
    assert!(
        expected_token.matches(&presented),
        "client must re-read the current service token"
    );
    ws.send(Message::Text(
        serde_json::to_string(&ServerMessage::Authenticated {
            capabilities: Vec::new(),
        })
        .unwrap(),
    ))
    .await
    .expect("authentication acknowledgement writes");
    let snapshot: ClientMessage = serde_json::from_str(
        &ws.next()
            .await
            .expect("client requests a snapshot")
            .expect("snapshot frame is valid")
            .into_text()
            .expect("snapshot is text"),
    )
    .expect("snapshot JSON parses");
    assert_eq!(snapshot, ClientMessage::RequestSnapshot);
    ws
}

#[tokio::test]
async fn shell_messages_rehydrate_external_tray_and_notice_feeds() {
    let (tray_tx, mut tray_rx) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, mut notice_rx) = broadcast::channel(4);
    let (pause_tx, mut pause_rx) = pause_channel();

    forward_shell_message(
        &ServerMessage::TrayState {
            state: BridgeTrayState::Pending(2),
        },
        1,
        &tray_tx,
        &notice_tx,
        &pause_tx,
    );
    tray_rx.changed().await.expect("tray sender is alive");
    assert_eq!(tray_rx.borrow().connection_id, 1);
    assert_eq!(tray_rx.borrow().state, BridgeTrayState::Pending(2));

    let notice = BridgeNotice::Pending {
        row_id: 17,
        process: "firefox".into(),
    };
    forward_shell_message(
        &ServerMessage::Notice {
            notice: notice.clone(),
        },
        1,
        &tray_tx,
        &notice_tx,
        &pause_tx,
    );
    let received = notice_rx.recv().await.unwrap();
    assert_eq!(received.connection_id, 1);
    assert_eq!(received.notice, notice);

    // Issue #47: the pause state and its end time reach the tray too.
    forward_shell_message(
        &ServerMessage::FilterPauseState {
            paused: true,
            expires_at_unix_ms: Some(1_800_000_300_000),
        },
        1,
        &tray_tx,
        &notice_tx,
        &pause_tx,
    );
    assert!(
        pause_rx.has_changed().unwrap(),
        "FilterPauseState was not routed to the pause feed"
    );
    let received = pause_rx.borrow_and_update().clone();
    assert_eq!(received.connection_id, 1);
    assert_eq!(
        received.state,
        BridgePauseState {
            paused: true,
            expires_at_unix_ms: Some(1_800_000_300_000),
        }
    );
}

fn pause_channel() -> (
    watch::Sender<ReceivedPauseState>,
    watch::Receiver<ReceivedPauseState>,
) {
    watch::channel(ReceivedPauseState {
        connection_id: 0,
        state: BridgePauseState::NOT_PAUSED,
    })
}

/// `BridgeFeed.appBoundRulesFor` (inline-Deny plan, version skew): only a row
/// of the live session whose bridge advertised app-bound rules may get a
/// remembered inline Deny. An always-true answer would fail open on old
/// bridges.
#[tokio::test]
async fn app_bound_rules_for_row_answers_only_for_the_live_capable_session() {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    };
    let for_row = crate::bridge_feed::app_bound_rules_for_row;

    mark_connected(&connection, true);
    assert!(for_row(Some(&handles), "1:7"));
    for row_id in ["2:7", "7", "0:7"] {
        assert!(!for_row(Some(&handles), row_id), "{row_id}");
    }
    assert!(!for_row(None, "1:7"), "no runtime");

    disconnect_and_discard(&connection, &mut inbound_rx);
    assert!(!for_row(Some(&handles), "1:7"), "after a disconnect");
}

#[tokio::test]
async fn disconnect_discards_queued_actions_and_rejects_new_ones() {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    };

    mark_connected(&connection, false);
    handles
        .try_send(ClientMessage::RecheckDiagnostics)
        .expect("the live session accepts a recheck");
    disconnect_and_discard(&connection, &mut inbound_rx);

    assert!(!handles.is_connected());
    assert!(
        !handles.is_current_session(1),
        "a queued Qt callback from the disconnected session must be dropped"
    );
    assert!(matches!(
        handles.try_send(ClientMessage::RecheckDiagnostics),
        Err(SendClientMessageError::Disconnected)
    ));
    assert!(matches!(
        inbound_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    // A subsequent bridge session gets its own connection id. The stale
    // diagnostics request above is gone rather than replayed here.
    mark_connected(&connection, false);
    assert!(handles.is_current_session(2));
    let verdict = |row_id| {
        crate::pending_decision::build_verdict_message(row_id, "deny", "this_host", "this_time")
            .unwrap()
    };
    // Both generic JSON and typed QML submissions converge on dispatch_to.
    // Held-open dialogs retain 1:1 even after new service rows reuse ID 1.
    assert_eq!(
        crate::bridge_feed::dispatch_to(&handles, verdict("1:1"), true),
        Err(SendClientMessageError::StaleSession)
    );
    assert_eq!(
        crate::bridge_feed::dispatch_to(&handles, verdict("1"), true),
        Err(SendClientMessageError::StaleSession)
    );
    assert!(matches!(
        inbound_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    crate::bridge_feed::dispatch_to(&handles, verdict("2:1"), true)
        .expect("new snapshot's identical wire row ID remains actionable");
    let fresh_verdict = inbound_rx.recv().await.unwrap();
    assert_eq!(fresh_verdict.connection_id, 2);
    assert!(
        matches!(fresh_verdict.message, ClientMessage::SetVerdict { row_id, .. } if row_id == "1")
    );
    assert!(
        !handles.is_current_session(1),
        "a reconnect must not make the prior session current again"
    );
    handles
        .try_send(ClientMessage::RecheckDiagnostics)
        .expect("the replacement session accepts a fresh recheck");
    assert_eq!(
        inbound_rx
            .recv()
            .await
            .expect("fresh request queued")
            .connection_id,
        2
    );
}

#[tokio::test]
async fn client_loop_forwards_authenticated_snapshot_to_the_qml_feed() {
    let dir = tempfile::tempdir().unwrap();
    let config = snitchwatch_bridge_cli::BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = snitchwatch_bridge_cli::run(config.clone())
        .await
        .expect("bridge starts");
    // Room for the whole snapshot, which grows whenever the bridge adds one.
    let (shell_tx, mut shell_messages) = broadcast::channel(64);
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::default()));
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let (slot_tx, slot_rx) = watch::channel(ReceivedPromptSlot::default());
    let client = tokio::spawn(client_loop(
        config.ws_socket_path.clone(),
        shell_tx,
        inbound_rx,
        status,
        ShellFeeds {
            tray_tx,
            notice_tx,
            pause_tx: pause_channel().0,
            slot_tx,
        },
        connection.clone(),
    ));

    // This verifies the production external-client path rather than only
    // the bridge's internal broadcast. An empty authenticated service
    // answers RequestSnapshot with messages that reach the QML-facing
    // shell feed.
    let mut saw_clear = false;
    let mut saw_blocklists = false;
    let mut saw_profiles = false;
    let mut saw_tray = false;
    let mut saw_slot = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !(saw_clear && saw_blocklists && saw_profiles && saw_tray && saw_slot) {
        match tokio::time::timeout_at(deadline, shell_messages.recv())
            .await
            .expect("client did not forward the authenticated snapshot")
            .expect("QML-facing shell feed closed")
        {
            ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::ClearConnectionRows,
            } => saw_clear = true,
            ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::SetBlocklists { .. },
            } => saw_blocklists = true,
            ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::SetProfiles { .. },
            } => saw_profiles = true,
            ReceivedServerMessage {
                connection_id: 1,
                message:
                    ServerMessage::TrayState {
                        state: BridgeTrayState::Idle,
                    },
            } => saw_tray = true,
            ReceivedServerMessage {
                connection_id: 1,
                message: ServerMessage::PromptSlot { .. },
            } => saw_slot = true,
            _ => {}
        }
    }
    assert!(is_current_connection(&connection, 1));
    // The snapshot's PromptSlot also reached the shell's slot feed (a free
    // slot: nothing is asking), labelled with its session.
    let slot = slot_rx.borrow().clone();
    assert_eq!(
        (slot.connection_id, slot.holder, slot.holders),
        (1, None, 0)
    );

    drop(inbound_tx);
    client.abort();
    bridge.shutdown();
}

#[tokio::test]
async fn a_server_message_this_client_cannot_parse_is_skipped() {
    // Code review M1 on #47: a bridge newer than this client may send an
    // action it doesn't know. Dropping the connection for it would make
    // the client reconnect forever, cancelling pending prompts each time.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&token, &dir.path().join("token")).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let server =
        tokio::spawn(async move { accept_authenticated_snapshot_on(&listener, &token).await });

    let (broadcast_tx, _) = broadcast::channel(4);
    let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
    let (tray_tx, mut tray_rx) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::default()));
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let client = tokio::spawn(async move {
        connect_and_relay(
            &socket_path,
            &broadcast_tx,
            &mut inbound_rx,
            &status,
            &ShellFeeds {
                tray_tx,
                notice_tx,
                pause_tx: pause_channel().0,
                slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
            },
            &connection,
        )
        .await
    });

    let mut ws = server.await.unwrap();
    for frame in [
        r#"{"action":"someFutureAction","detail":1}"#.to_string(),
        serde_json::to_string(&ServerMessage::TrayState {
            state: BridgeTrayState::Pending(4),
        })
        .unwrap(),
    ] {
        ws.send(Message::Text(frame)).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), tray_rx.changed())
        .await
        .expect("the frame after the unknown one never arrived")
        .unwrap();
    assert_eq!(tray_rx.borrow().connection_id, 1);
    assert_eq!(tray_rx.borrow().state, BridgeTrayState::Pending(4));
    assert!(!client.is_finished(), "the client dropped the connection");

    drop(inbound_tx);
    client.await.unwrap().unwrap();
}

/// Accept one client, check its token, acknowledge it and take its
/// snapshot request; hand back the server side of the socket. The literal
/// bare acknowledgement every bridge before capabilities sent.
async fn accept_authenticated_snapshot_on(
    listener: &tokio::net::UnixListener,
    token: &snitchwatch_bridge::auth::Token,
) -> tokio_tungstenite::WebSocketStream<tokio::net::UnixStream> {
    accept_with_ack(listener, token, r#"{"action":"authenticated"}"#).await
}

/// [`accept_authenticated_snapshot_on`] with the acknowledgement frame given.
async fn accept_with_ack(
    listener: &tokio::net::UnixListener,
    token: &snitchwatch_bridge::auth::Token,
    ack: &str,
) -> tokio_tungstenite::WebSocketStream<tokio::net::UnixStream> {
    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    let presented = ws.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(token.matches(&presented));
    ws.send(Message::Text(ack.to_string())).await.unwrap();
    let snapshot: ClientMessage =
        serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(snapshot, ClientMessage::RequestSnapshot);
    ws
}

/// Waits until `connection_id` is (or, with `live == false`, is no longer)
/// the live session.
async fn wait_for_session(connection: &Mutex<ConnectionState>, connection_id: u64, live: bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while is_current_connection(connection, connection_id) != live {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("session state did not change");
}

/// Inline-Deny plan, version skew: a bridge older than #50/#71 builds "This
/// host" rules for every app. Only a session whose own acknowledgement
/// advertised app-bound rules may get a remembered inline Deny; a reconnect to
/// an older bridge (the literal bare acknowledgement) must not inherit it.
#[tokio::test]
async fn app_bound_rules_follow_each_sessions_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token_path = dir.path().join("token");
    let first_token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&first_token, &token_path).unwrap();
    let first_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let capable_ack = serde_json::to_string(&ServerMessage::Authenticated {
        capabilities: snitchwatch_bridge::bridge_capabilities::advertised(),
    })
    .unwrap();
    let (stop_first_tx, stop_first_rx) = tokio::sync::oneshot::channel::<()>();
    let first_server = tokio::spawn(async move {
        let ws = accept_with_ack(&first_listener, &first_token, &capable_ack).await;
        let _ = stop_first_rx.await;
        drop(ws);
    });

    let (broadcast_tx, _) = broadcast::channel(4);
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let client = tokio::spawn(client_loop(
        socket_path.clone(),
        broadcast_tx,
        inbound_rx,
        Arc::new(Mutex::new(LinkStatus::default())),
        ShellFeeds {
            tray_tx,
            notice_tx,
            pause_tx: pause_channel().0,
            slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
        },
        connection.clone(),
    ));

    wait_for_session(&connection, 1, true).await;
    assert!(advertises_app_bound_rules(&connection, 1));
    assert!(
        !advertises_app_bound_rules(&connection, 2),
        "only the advertising session"
    );
    assert!(
        connection.lock().unwrap().pause_answers_waiting,
        "the capable bridge's acknowledgement advertises pauseAnswersWaiting"
    );

    stop_first_tx.send(()).unwrap();
    first_server.await.unwrap();
    wait_for_session(&connection, 1, false).await;
    assert!(!advertises_app_bound_rules(&connection, 1));
    assert!(
        !connection.lock().unwrap().app_bound_rules,
        "a disconnect clears the flag itself"
    );
    assert!(
        !connection.lock().unwrap().pause_answers_waiting,
        "a disconnect clears the pause flag too"
    );

    std::fs::remove_file(&socket_path).unwrap();
    let second_token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&second_token, &token_path).unwrap();
    let second_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let second_server = tokio::spawn(async move {
        let _ws = accept_authenticated_snapshot_on(&second_listener, &second_token).await;
        std::future::pending::<()>().await;
    });

    wait_for_session(&connection, 2, true).await;
    assert!(
        !advertises_app_bound_rules(&connection, 2),
        "an older bridge's bare acknowledgement advertises nothing"
    );
    assert!(
        !connection.lock().unwrap().pause_answers_waiting,
        "an older bridge's bare acknowledgement doesn't promise the pause answers"
    );

    drop(inbound_tx);
    client.abort();
    second_server.abort();
}

#[tokio::test]
async fn client_stays_pending_until_service_acknowledges_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token_path = dir.path().join("token");
    let token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&token, &token_path).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let (token_received_tx, token_received_rx) = tokio::sync::oneshot::channel();
    let (release_ack_tx, release_ack_rx) = tokio::sync::oneshot::channel();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let presented = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(presented, token.as_str());
        token_received_tx.send(()).unwrap();

        release_ack_rx.await.unwrap();
        ws.send(Message::Text(
            serde_json::to_string(&ServerMessage::Authenticated {
                capabilities: Vec::new(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();

        let snapshot: ClientMessage =
            serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(snapshot, ClientMessage::RequestSnapshot);
        std::future::pending::<()>().await;
    });

    let (broadcast_tx, _) = broadcast::channel(4);
    let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::default()));
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let client_connection = connection.clone();
    let client_status = status.clone();
    let client = tokio::spawn(async move {
        connect_and_relay(
            &socket_path,
            &broadcast_tx,
            &mut inbound_rx,
            &client_status,
            &ShellFeeds {
                tray_tx,
                notice_tx,
                pause_tx: pause_channel().0,
                slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
            },
            &client_connection,
        )
        .await
    });

    token_received_rx.await.unwrap();
    assert!(
        !is_current_connection(&connection, 1),
        "sending the token alone must not expose a connected session"
    );
    assert_ne!(
        status.lock().unwrap().state,
        LinkState::Connected,
        "the status must remain pending until the acknowledgement arrives"
    );

    release_ack_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if is_current_connection(&connection, 1) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client should connect after the acknowledgement");

    drop(inbound_tx);
    client.await.unwrap().unwrap();
    server.abort();
}

#[tokio::test]
async fn client_loop_reconnects_after_service_socket_and_token_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token_path = dir.path().join("token");
    let first_token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&first_token, &token_path).unwrap();
    let first_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

    let (first_snapshot_tx, first_snapshot_rx) = tokio::sync::oneshot::channel();
    let first_server = tokio::spawn(async move {
        accept_authenticated_snapshot(&first_listener, &first_token).await;
        first_snapshot_tx.send(()).unwrap();
        // Dropping this listener and its WebSocket simulates the service
        // stopping. It deliberately does not leave a usable connection.
    });

    let (broadcast_tx, _) = broadcast::channel(4);
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::default()));
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let client = tokio::spawn(client_loop(
        socket_path.clone(),
        broadcast_tx,
        inbound_rx,
        status,
        ShellFeeds {
            tray_tx,
            notice_tx,
            pause_tx: pause_channel().0,
            slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
        },
        connection.clone(),
    ));

    tokio::time::timeout(Duration::from_secs(2), first_snapshot_rx)
        .await
        .expect("first service generation receives a snapshot")
        .unwrap();
    first_server.await.unwrap();
    std::fs::remove_file(&socket_path).unwrap();

    let second_token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&second_token, &token_path).unwrap();
    let second_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let (second_snapshot_tx, second_snapshot_rx) = tokio::sync::oneshot::channel();
    let second_server = tokio::spawn(async move {
        accept_authenticated_snapshot(&second_listener, &second_token).await;
        second_snapshot_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });

    tokio::time::timeout(Duration::from_secs(3), second_snapshot_rx)
        .await
        .expect("replacement service receives a fresh snapshot")
        .unwrap();
    assert!(is_current_connection(&connection, 2));

    // `client_loop` is intentionally long-lived while its runtime owns
    // it; abort the test task after proving the replacement connection.
    drop(inbound_tx);
    client.abort();
    second_server.abort();
}

#[tokio::test]
async fn client_loop_recovers_from_missing_and_stale_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token_path = dir.path().join("token");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let stale_token = snitchwatch_bridge::auth::Token::generate();
    let current_token = snitchwatch_bridge::auth::Token::generate();
    let server_stale_token = stale_token.clone();
    let server_current_token = current_token.clone();
    let (stale_seen_tx, stale_seen_rx) = tokio::sync::oneshot::channel();
    let (recovered_tx, recovered_rx) = tokio::sync::oneshot::channel();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stale_ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let stale_presented = stale_ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(server_stale_token.matches(&stale_presented));
        stale_seen_tx.send(()).unwrap();
        // A real service rejects a stale token by closing before its ack.
        stale_ws.close(None).await.unwrap();

        accept_authenticated_snapshot(&listener, &server_current_token).await;
        recovered_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });

    let (broadcast_tx, _) = broadcast::channel(4);
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::default()));
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let client = tokio::spawn(client_loop(
        socket_path,
        broadcast_tx,
        inbound_rx,
        status,
        ShellFeeds {
            tray_tx,
            notice_tx,
            pause_tx: pause_channel().0,
            slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
        },
        connection,
    ));

    // Let the first attempt observe the absent file. Its next attempt
    // reads this stale generation and is rejected by the service.
    tokio::time::sleep(Duration::from_millis(50)).await;
    snitchwatch_bridge::auth::write_token_file(&stale_token, &token_path).unwrap();
    tokio::time::timeout(Duration::from_secs(3), stale_seen_rx)
        .await
        .expect("client retries once a token appears")
        .unwrap();
    snitchwatch_bridge::auth::write_token_file(&current_token, &token_path).unwrap();

    tokio::time::timeout(Duration::from_secs(3), recovered_rx)
        .await
        .expect("client recovers after token rotation")
        .unwrap();
    drop(inbound_tx);
    client.abort();
    server.abort();
}

#[test]
fn production_client_runtime_cannot_take_over_service_resources() {
    // Keep this narrow and intentional: test only the production section,
    // so test fixtures may still start a bridge service in-process.
    let production = include_str!("../bridge_runtime.rs")
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    for forbidden in [
        "snitchwatch_bridge_cli::run(",
        "UnixListener::bind(",
        "write_token_file(",
        "remove_file(",
        "127.0.0.1:50051",
    ] {
        assert!(
            !production.contains(forbidden),
            "external Kirigami client must not contain {forbidden}"
        );
    }
    let entrypoint = include_str!("../main.rs");
    assert!(
        !entrypoint.contains("snitchwatch_bridge_cli"),
        "the production binary must only start the external client runtime"
    );
}

// ---- the link state the bridge banner switches on --------------------------

fn shell_feeds() -> ShellFeeds {
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(1);
    ShellFeeds {
        tray_tx,
        notice_tx,
        pause_tx: pause_channel().0,
        slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
    }
}

async fn wait_for_state(status: &Arc<Mutex<LinkStatus>>, wanted: LinkState) -> LinkStatus {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let link = status.lock().unwrap().clone();
            if link.state == wanted {
                return link;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the link never reached {wanted:?}; it is {:?}",
            status.lock().unwrap()
        )
    })
}

#[test]
fn link_states_have_the_fixed_tokens_the_banner_switches_on() {
    let tokens: Vec<&str> = [
        LinkState::Connecting,
        LinkState::Connected,
        LinkState::Retrying,
        LinkState::Failed,
        LinkState::Stopped,
    ]
    .into_iter()
    .map(LinkState::token)
    .collect();
    assert_eq!(
        tokens,
        ["connecting", "connected", "retrying", "failed", "stopped"]
    );
}

#[test]
fn the_runtime_starts_out_connecting() {
    let link = LinkStatus::connecting();
    assert_eq!(link.state, LinkState::Connecting);
    assert_eq!(LinkStatus::default().state, LinkState::Connecting);
}

#[test]
fn a_runtime_that_could_not_start_is_failed_and_not_ok() {
    let outcome = Outcome::Failed("no async runtime".to_string());
    let link = link_of(&outcome);
    assert_eq!(link.state, LinkState::Failed);
    assert!(link.detail.contains("no async runtime"), "{link:?}");
    assert_eq!(status_of(&outcome), (false, link.detail));
}

#[test]
fn only_the_connected_state_is_ok() {
    for (state, ok) in [
        (LinkState::Connecting, false),
        (LinkState::Connected, true),
        (LinkState::Retrying, false),
        (LinkState::Failed, false),
        (LinkState::Stopped, false),
    ] {
        let link = LinkStatus {
            state,
            detail: "Connected to bridge service".to_string(),
        };
        assert_eq!(link.state == LinkState::Connected, ok, "{state:?}");
    }
}

#[tokio::test]
async fn a_service_that_is_not_there_leaves_the_client_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::connecting()));
    let client = tokio::spawn(client_loop(
        dir.path().join("no-such.sock"),
        broadcast_tx,
        inbound_rx,
        status.clone(),
        shell_feeds(),
        Arc::new(Mutex::new(ConnectionState::default())),
    ));

    let link = wait_for_state(&status, LinkState::Retrying).await;
    assert!(link.detail.starts_with("Bridge unavailable"), "{link:?}");
    // It keeps trying: the task has not finished.
    assert!(!client.is_finished());

    drop(inbound_tx);
    client.abort();
}

#[tokio::test]
async fn a_connected_client_is_connected_and_stopped_once_its_sender_goes_away() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&token, &dir.path().join("token")).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let server = tokio::spawn(async move {
        // Keep the connection open: the service is up.
        let _connection = accept_authenticated_snapshot(&listener, &token).await;
        std::future::pending::<()>().await;
    });

    let (broadcast_tx, _) = broadcast::channel(4);
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let status = Arc::new(Mutex::new(LinkStatus::connecting()));
    let client = tokio::spawn(client_loop(
        socket_path,
        broadcast_tx,
        inbound_rx,
        status.clone(),
        shell_feeds(),
        Arc::new(Mutex::new(ConnectionState::default())),
    ));

    wait_for_state(&status, LinkState::Connected).await;
    drop(inbound_tx);
    tokio::time::timeout(Duration::from_secs(3), client)
        .await
        .expect("the client stops once nothing can send to it")
        .unwrap();
    let link = status.lock().unwrap().clone();
    assert_eq!(link.state, LinkState::Stopped, "{link:?}");
    server.abort();
}
