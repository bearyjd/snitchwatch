use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message as TMessage;

fn socket_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("bridge.sock")
}

fn default_handles() -> WsHandles {
    let (broadcast_tx, _) = broadcast::channel(16);
    let (inbound_tx, _) = mpsc::channel(16);
    let store = Arc::new(crate::blocklists::store::BlocklistStore::open_in_memory().unwrap());
    let profile_store = Arc::new(crate::profiles::store::ProfileStore::open_in_memory().unwrap());
    WsHandles {
        broadcast: broadcast_tx,
        presence: Default::default(),
        inbound: inbound_tx,
        blocklists: Arc::new(BlocklistsManager::new(store)),
        profiles: Arc::new(crate::profiles::ProfilesManager::new(profile_store)),
    }
}

/// Spawn a `WsServer` bound to a fresh Unix socket in `dir`, returning
/// the socket path plus the broadcast sender / inbound receiver so tests
/// can drive and observe it.
async fn spawn_server_with_inbound(
    dir: &tempfile::TempDir,
    token: Token,
) -> (
    PathBuf,
    broadcast::Sender<ServerMessage>,
    mpsc::Receiver<ClientMessage>,
    tokio::task::JoinHandle<()>,
) {
    let (broadcast_tx, _) = broadcast::channel(16);
    let (inbound_tx, inbound_rx) = mpsc::channel(16);
    let store = Arc::new(crate::blocklists::store::BlocklistStore::open_in_memory().unwrap());
    let profile_store = Arc::new(crate::profiles::store::ProfileStore::open_in_memory().unwrap());
    let handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        presence: Default::default(),
        inbound: inbound_tx,
        blocklists: Arc::new(BlocklistsManager::new(store)),
        profiles: Arc::new(crate::profiles::ProfilesManager::new(profile_store)),
    };

    let path = socket_path(dir);
    let server = WsServer::new(path.clone(), token, handles);
    let listener = server.bind().await.expect("bind should succeed");
    let join = tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    (path, broadcast_tx, inbound_rx, join)
}

async fn connect(path: &PathBuf) -> tokio_tungstenite::WebSocketStream<UnixStream> {
    // Retry briefly: the listener may not have finished binding yet
    // since `spawn_server` returns as soon as the spawn is scheduled.
    let mut last_err = None;
    for _ in 0..50 {
        match UnixStream::connect(path).await {
            Ok(stream) => {
                let (ws, _resp) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
                    .await
                    .expect("ws handshake should succeed");
                return ws;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }
    panic!("failed to connect to unix socket: {last_err:?}");
}

#[tokio::test]
async fn only_acknowledged_authenticated_clients_establish_presence() {
    let dir = tempfile::tempdir().unwrap();
    let handles = default_handles();
    let presence = handles.presence.clone();
    let token = Token::generate();
    let path = socket_path(&dir);
    let server = WsServer::new(path.clone(), token.clone(), handles);
    let listener = server.bind().await.unwrap();
    let server = tokio::spawn(server.serve(listener));
    let mut stalled = connect(&path).await;
    assert!(presence.admit().is_none());
    let mut invalid = connect(&path).await;
    invalid
        .send(TMessage::Text("invalid-token".into()))
        .await
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), invalid.next())
        .await
        .unwrap();
    assert!(presence.admit().is_none());
    let mut first = connect(&path).await;
    first
        .send(TMessage::Text(token.as_str().into()))
        .await
        .unwrap();
    let ack = first.next().await.unwrap().unwrap();
    assert!(matches!(ack, TMessage::Text(ref text) if text.contains("authenticated")));
    let mut second = connect(&path).await;
    second
        .send(TMessage::Text(token.as_str().into()))
        .await
        .unwrap();
    let _ = second.next().await.unwrap().unwrap();
    let mut admitted = presence.admit().unwrap();
    first.close(None).await.unwrap();
    // Second authenticated client keeps the request admitted; stalled
    // and invalid transports must not affect this count.
    tokio::task::yield_now().await;
    assert!(admitted.while_current(|| ()).is_some());
    second.close(None).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), admitted.lost())
        .await
        .unwrap();
    assert!(presence.admit().is_none());
    stalled.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn outbound_failure_releases_presence_with_stalled_inbound() {
    let handles = default_handles();
    let presence = handles.presence.clone();
    let broadcast = handles.broadcast.clone();
    let failing_sink = Box::pin(futures_util::sink::unfold(
        (),
        |(), _message: Message| async {
            Err::<(), std::io::Error>(std::io::Error::other("outbound failed"))
        },
    ));
    let never_receives = futures_util::stream::pending::<Result<Message, axum::Error>>();
    let pump = tokio::spawn(pump_authenticated(
        failing_sink,
        never_receives,
        handles,
        None,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while presence.admit().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut admission = presence.admit().unwrap();
    broadcast
        .send(ServerMessage::Authenticated {
            capabilities: Vec::new(),
        })
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), pump)
        .await
        .unwrap()
        .unwrap();
    admission.lost().await;
    assert!(presence.admit().is_none());
}

#[tokio::test]
async fn pause_requests_carry_the_sender_session_generation() {
    // Issue #47: the stamp is what lets `apply_pause_request` ignore a
    // pause queued by a GUI whose generation has ended. A client-supplied
    // value must never survive.
    let dir = tempfile::tempdir().unwrap();
    let (inbound_tx, mut inbound_rx) = mpsc::channel(16);
    let handles = WsHandles {
        inbound: inbound_tx,
        ..default_handles()
    };
    let presence = handles.presence.clone();
    // Move past generation 0, so a default value can't pass by accident.
    drop(presence.authenticated_session());
    let token = Token::generate();
    let path = socket_path(&dir);
    let server = WsServer::new(path.clone(), token.clone(), handles);
    let listener = server.bind().await.unwrap();
    let server = tokio::spawn(server.serve(listener));

    let mut gui = connect(&path).await;
    gui.send(TMessage::Text(token.as_str().into()))
        .await
        .unwrap();
    let _ack = gui.next().await.unwrap().unwrap();
    gui.send(TMessage::Text(
        r#"{"action":"setFilteringPaused","paused":true,"durationSecs":1800,"senderGeneration":999}"#
            .into(),
    ))
    .await
    .unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(2), inbound_rx.recv())
        .await
        .expect("pause request was not forwarded")
        .unwrap();
    assert_eq!(presence.current_generation(), 1);
    use std::os::unix::fs::MetadataExt;
    let own_uid = fs::metadata(dir.path()).unwrap().uid();
    assert_eq!(
        received,
        ClientMessage::SetFilteringPaused {
            paused: true,
            duration_secs: Some(1800),
            sender_generation: Some(1),
            sender_uid: Some(own_uid),
        }
    );
    server.abort();
}

#[tokio::test]
async fn server_state_carries_blocklists_manager() {
    use crate::blocklists::store::BlocklistStore;
    use crate::blocklists::BlocklistsManager;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mgr = Arc::new(BlocklistsManager::new(store));
    let handles = default_handles();
    let server =
        WsServer::new_with_blocklists(socket_path(&dir), Token::generate(), handles, mgr.clone());
    assert!(Arc::ptr_eq(server.blocklists(), &mgr));
}

#[tokio::test]
async fn server_binds_unix_socket_with_expected_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let handles = default_handles();
    let path = socket_path(&dir);
    let server = WsServer::new(path.clone(), Token::generate(), handles);
    let _listener = server.bind().await.expect("bind should succeed");

    assert!(path.exists(), "socket file should exist after bind");
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket file should be mode 0600");

    let parent_mode = fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(parent_mode, 0o700, "parent dir should be mode 0700");
}

#[tokio::test]
async fn connection_without_token_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let token = Token::generate();
    let (path, _broadcast_tx, mut inbound_rx, _join) = spawn_server_with_inbound(&dir, token).await;

    let mut ws = connect(&path).await;

    // Send a ClientMessage-shaped frame *without* presenting the token
    // first — this should be consumed as the (failed) handshake attempt
    // and the connection closed, never reaching `handles.inbound`.
    ws.send(TMessage::Text(
        serde_json::json!({
            "action": "setVerdict",
            "rowId": "ask-1",
            "verdict": "allow",
            "scope": "this_host",
            "duration": "once"
        })
        .to_string(),
    ))
    .await
    .expect("send should not fail at the transport level");

    // The server should close the connection right after rejecting the
    // handshake.
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
    match next {
        Ok(Some(Ok(TMessage::Close(_)))) | Ok(None) => {}
        other => panic!("expected connection close after failed handshake, got {other:?}"),
    }

    let got_inbound =
        tokio::time::timeout(std::time::Duration::from_millis(200), inbound_rx.recv()).await;
    assert!(
        got_inbound.is_err(),
        "no ClientMessage should reach handles.inbound without a valid token"
    );
}

#[tokio::test]
async fn connection_with_wrong_token_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let token = Token::generate();
    let (path, _broadcast_tx, mut inbound_rx, _join) = spawn_server_with_inbound(&dir, token).await;

    let mut ws = connect(&path).await;
    ws.send(TMessage::Text("definitely-not-the-token".to_string()))
        .await
        .unwrap();

    let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
    match next {
        Ok(Some(Ok(TMessage::Close(_)))) | Ok(None) => {}
        other => panic!("expected connection close after wrong token, got {other:?}"),
    }

    let got_inbound =
        tokio::time::timeout(std::time::Duration::from_millis(200), inbound_rx.recv()).await;
    assert!(
        got_inbound.is_err(),
        "wrong token must not unlock the stream"
    );
}

#[tokio::test]
async fn connection_with_correct_token_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let token = Token::generate();
    let (path, broadcast_tx, mut inbound_rx, _join) =
        spawn_server_with_inbound(&dir, token.clone()).await;

    let mut ws = connect(&path).await;

    // 1. Present the token first.
    ws.send(TMessage::Text(token.as_str().to_string()))
        .await
        .unwrap();

    // The acknowledgement is deliberately the first server frame. A
    // client must wait for this rather than assuming its token write was
    // accepted.
    let acknowledgement = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("should receive authentication acknowledgement")
        .expect("stream should not end")
        .expect("frame should not error");
    // The acknowledgement advertises app-bound rules (inline-Deny plan):
    // Kirigami only remembers an inline Deny when it is present.
    assert!(matches!(
        acknowledgement,
        TMessage::Text(ref text)
            if matches!(
                serde_json::from_str(text),
                Ok(ServerMessage::Authenticated { ref capabilities })
                    if capabilities.iter().any(|c| c == crate::bridge_capabilities::APP_BOUND_RULES)
            )
    ));

    // 2. Now a real ClientMessage should reach `handles.inbound`.
    let verdict = serde_json::json!({
        "action": "setVerdict",
        "rowId": "ask-1",
        "verdict": "allow",
        "scope": "this_host",
        "duration": "once"
    });
    ws.send(TMessage::Text(verdict.to_string())).await.unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(2), inbound_rx.recv())
        .await
        .expect("should receive a ClientMessage")
        .expect("channel should not be closed");
    match received {
        ClientMessage::SetVerdict { row_id, .. } => assert_eq!(row_id, "ask-1"),
        other => panic!("expected SetVerdict, got {other:?}"),
    }

    // 3. Broadcast messages still flow to the client after handshake.
    broadcast_tx
        .send(ServerMessage::InsertConnectionRows { rows: vec![] })
        .unwrap();
    let outbound = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("should receive a broadcast message")
        .expect("stream should not end")
        .expect("frame should not error");
    match outbound {
        TMessage::Text(t) => {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            assert_eq!(
                v.get("action").and_then(|a| a.as_str()),
                Some("insertConnectionRows")
            );
        }
        other => panic!("expected a text frame, got {other:?}"),
    }
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn server_serves_index_html_at_root_after_handshake_token_gate() {
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    let handles = default_handles();
    let state = AppState {
        handles,
        token: Token::generate(),
    };

    let app = WsServer::router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(std::str::from_utf8(&body).unwrap().contains("Snitchwatch"));
}

#[cfg(feature = "web-ui")]
#[tokio::test]
async fn server_serves_asset_js_unauthenticated() {
    use axum::http::Request;
    use tower::ServiceExt;

    let handles = default_handles();
    let state = AppState {
        handles,
        token: Token::generate(),
    };
    let app = WsServer::router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/assets/js/app.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

/// The release tarball's build (`--no-default-features`): no embedded
/// frontend, so the static routes and the SPA fallback are all 404s.
#[cfg(not(feature = "web-ui"))]
#[tokio::test]
async fn without_web_ui_static_routes_are_not_served() {
    use axum::http::Request;
    use tower::ServiceExt;

    for uri in ["/", "/assets/js/app.js", "/some/spa/route"] {
        let state = AppState {
            handles: default_handles(),
            token: Token::generate(),
        };
        let response = WsServer::router(state)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            404,
            "{uri} must not be served without web-ui"
        );
    }
}

/// Issue #45 (S5): a client frame over [`MAX_CLIENT_MESSAGE_BYTES`] ends the
/// connection instead of being buffered and parsed (axum's default is
/// 64 MiB).
#[tokio::test]
async fn an_oversized_client_message_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let token = Token::generate();
    let (path, _broadcast_tx, mut inbound_rx, _join) =
        spawn_server_with_inbound(&dir, token.clone()).await;
    let mut ws = connect(&path).await;
    ws.send(TMessage::Text(token.as_str().to_string()))
        .await
        .unwrap();
    let _ack = ws.next().await;

    let huge = serde_json::json!({
        "action": "subscribeBlocklist",
        "url": format!("https://x.example/{}", "a".repeat(MAX_CLIENT_MESSAGE_BYTES)),
    });
    let _ = ws.send(TMessage::Text(huge.to_string())).await;
    let received =
        tokio::time::timeout(std::time::Duration::from_millis(500), inbound_rx.recv()).await;
    assert!(
        !matches!(received, Ok(Some(_))),
        "an oversized message reached the bridge"
    );
    // A normal-size message right after it still fits the limit.
    let small = serde_json::json!({ "action": "undo" });
    assert!(MAX_CLIENT_MESSAGE_BYTES > small.to_string().len());
}

/// Rule import (P2.7): `pump_authenticated` checks a text frame's length
/// before `from_str::<ClientMessage>`, even when the transport's own limit
/// was bypassed, and the session keeps going.
#[tokio::test]
async fn pump_drops_an_oversized_frame_before_parsing_it() {
    let (inbound_tx, mut inbound_rx) = mpsc::channel(16);
    let handles = WsHandles {
        inbound: inbound_tx,
        ..default_handles()
    };
    // A well-formed ClientMessage: it would be forwarded if it were parsed.
    let oversized = serde_json::json!({
        "action": "previewRulesImport",
        "document": { "pad": "a".repeat(MAX_CLIENT_MESSAGE_BYTES) },
    })
    .to_string();
    assert!(oversized.len() > MAX_CLIENT_MESSAGE_BYTES);
    let frames = futures_util::stream::iter(vec![
        Ok(Message::Text(oversized)),
        Ok(Message::Text(r#"{"action":"undo"}"#.to_string())),
    ])
    .chain(futures_util::stream::pending());
    let sink = Box::pin(futures_util::sink::drain::<Message>());
    let pump = tokio::spawn(pump_authenticated(sink, Box::pin(frames), handles, None));

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), inbound_rx.recv())
        .await
        .expect("the session ended")
        .unwrap();
    assert!(
        matches!(first, ClientMessage::Undo),
        "the oversized frame was parsed"
    );
    pump.abort();
}

/// Rule import/export (P2.7 review #8): an import request is stamped with a
/// channel back to its own connection, and what is sent on it reaches that
/// connection only, never the broadcast.
#[tokio::test]
async fn import_requests_carry_a_reply_channel_to_their_connection() {
    let (inbound_tx, mut inbound_rx) = mpsc::channel(16);
    let handles = WsHandles {
        inbound: inbound_tx,
        ..default_handles()
    };
    let frames = futures_util::stream::iter(vec![Ok(Message::Text(
        r#"{"action":"exportRules","requestId":"r1"}"#.to_string(),
    ))])
    .chain(futures_util::stream::pending());
    let (sink_tx, mut sink_rx) = mpsc::channel::<Message>(16);
    let sink = Box::pin(futures_util::sink::unfold(
        sink_tx,
        |tx, message: Message| async move {
            tx.send(message)
                .await
                .map_err(|_| std::io::Error::other("closed"))?;
            Ok::<_, std::io::Error>(tx)
        },
    ));
    let pump = tokio::spawn(pump_authenticated(sink, Box::pin(frames), handles, None));

    let request = tokio::time::timeout(std::time::Duration::from_secs(2), inbound_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let ClientMessage::ExportRules {
        request_id,
        reply: Some(reply),
    } = request
    else {
        panic!("no reply channel: {request:?}");
    };
    assert_eq!(request_id, "r1");
    assert!(
        reply
            .send(ServerMessage::RulesExportUnavailable {
                request_id: "r1".into(),
                reason: "x".into(),
            })
            .await
    );
    let Message::Text(text) =
        tokio::time::timeout(std::time::Duration::from_secs(2), sink_rx.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("not text")
    };
    assert!(
        text.contains("rulesExportUnavailable") && text.contains("r1"),
        "{text}"
    );
    pump.abort();
}
