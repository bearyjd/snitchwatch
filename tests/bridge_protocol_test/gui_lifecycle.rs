use super::*;

#[tokio::test]
async fn no_gui_ask_is_unavailable_despite_internal_bridge_consumers() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    })
    .await
    .unwrap();
    let mut observer = bridge.broadcast_tx.subscribe();
    let mut client = snitchwatch_proto::protocol::ui_client::UiClient::connect(format!(
        "http://{}",
        bridge.grpc_endpoint.tcp_addr().unwrap()
    ))
    .await
    .unwrap();
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        client.ask_rule(Connection::default()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);
    while let Ok(message) = observer.try_recv() {
        assert!(!matches!(
            message,
            ServerMessage::InsertConnectionRows { .. }
        ));
    }
    bridge.shutdown();
}

#[tokio::test]
async fn last_real_gui_disconnect_removes_prompt_and_rejects_late_persistent_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    })
    .await
    .unwrap();
    let mut observer = bridge.broadcast_tx.subscribe();
    let mut gui = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;
    let mut client = snitchwatch_proto::protocol::ui_client::UiClient::connect(format!(
        "http://{}",
        bridge.grpc_endpoint.tcp_addr().unwrap()
    ))
    .await
    .unwrap();
    let ask = tokio::spawn(async move {
        client
            .ask_rule(Connection {
                dst_host: "disconnect.test".into(),
                dst_ip: "192.0.2.15".into(),
                dst_port: 8443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
    });
    let id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerMessage::InsertConnectionRows { rows } = observer.recv().await.unwrap() {
                break rows[0].id.clone();
            }
        }
    })
    .await
    .unwrap();
    gui.close(None).await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerMessage::RemoveConnectionRows { ids } = observer.recv().await.unwrap() {
                if ids.contains(&id) {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);
    let mut reconnected = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;
    reconnected
        .send(Message::Text(
            json!({
                "action": "setVerdict", "rowId": id, "verdict": "allow",
                "scope": "any_host", "duration": "always"
            })
            .to_string(),
        ))
        .await
        .unwrap();
    reconnected
        .send(Message::Text(
            json!({"action": "requestSnapshot"}).to_string(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut cleared = false;
        loop {
            match observer.recv().await.unwrap() {
                ServerMessage::ClearConnectionRows => cleared = true,
                ServerMessage::InsertConnectionRows { rows } => assert!(rows.is_empty()),
                ServerMessage::UpdateRules { .. } | ServerMessage::UpdateConnectionRows { .. } => {
                    panic!("late verdict produced rule/history effects")
                }
                ServerMessage::DiagnosticsReport { .. } if cleared => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    bridge.shutdown();
}

/// Issue #49 ("stuck pending rows"), end to end: opensnitchd gives every
/// `AskRule` a 120 s context deadline and then applies its default action.
/// The bridge learns the prompt is dead only because its handler future is
/// dropped (`grpc_server::PendingCleanup`), as when a grpc-go call is
/// cancelled. The GUI here is authenticated but never answers, which is
/// exactly what left the row pending before #39.
///
/// Asserts what that GUI is shown: the row appears, then is removed, and the
/// tray returns to `Idle`. A verdict that arrives after the removal must
/// change nothing: no `UpdateConnectionRows`, no `UpdateRules`.
///
/// The deadline is 300 ms rather than 120 s. `MockOpensnitchd::
/// ask_rule_with_deadline` sends it as a `grpc-timeout` header, which tonic's
/// timeout layer enforces by dropping the handler future (see that method),
/// so the same cleanup runs. It does not time out locally.
#[tokio::test]
async fn ask_rule_deadline_removes_row_for_silent_gui_and_rejects_late_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    })
    .await
    .unwrap();
    let mut gui = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;
    let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    let ask = tokio::spawn(async move {
        mock.ask_rule_with_deadline(
            Connection {
                dst_host: "deadline.test".into(),
                dst_ip: "192.0.2.49".into(),
                dst_port: 8443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            },
            Duration::from_millis(300),
        )
        .await
    });

    let id = loop {
        if let ServerMessage::InsertConnectionRows { rows } = next_server_message(&mut gui).await {
            assert!(rows[0].action.is_none(), "the prompt starts out pending");
            break rows[0].id.clone();
        }
    };
    loop {
        match next_server_message(&mut gui).await {
            ServerMessage::RemoveConnectionRows { ids } if ids.contains(&id) => break,
            ServerMessage::UpdateConnectionRows { .. } | ServerMessage::UpdateRules { .. } => {
                panic!("an unanswered, timed-out Ask produced a verdict side effect")
            }
            _ => {}
        }
    }
    // The tray frame that follows the removal is what the tray icon shows.
    loop {
        if let ServerMessage::TrayState { state } = next_server_message(&mut gui).await {
            assert_eq!(state, TrayState::Idle);
            break;
        }
    }
    assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);

    let error = tokio::time::timeout(Duration::from_secs(10), ask)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    let mock_opensnitchd::MockError::Rpc(status) = error else {
        panic!("expected the deadline to cancel the RPC, got {error:?}");
    };
    assert!(matches!(
        status.code(),
        tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
    ));

    // A late, persistent verdict for the removed row, then a snapshot as the
    // barrier: the inbound pump handles frames in order, so once the snapshot's
    // diagnostics report arrives, the verdict has been handled.
    gui.send(Message::Text(
        json!({
            "action": "setVerdict", "rowId": id, "verdict": "allow",
            "scope": "any_host", "duration": "always"
        })
        .to_string(),
    ))
    .await
    .unwrap();
    gui.send(Message::Text(
        json!({"action": "requestSnapshot"}).to_string(),
    ))
    .await
    .unwrap();
    let mut cleared = false;
    loop {
        match next_server_message(&mut gui).await {
            ServerMessage::ClearConnectionRows => cleared = true,
            ServerMessage::InsertConnectionRows { rows } => {
                assert!(
                    rows.iter().all(|row| row.id != id),
                    "the removed row came back in the snapshot"
                );
            }
            ServerMessage::UpdateRules { .. } | ServerMessage::UpdateConnectionRows { .. } => {
                panic!("late verdict produced rule/history effects")
            }
            ServerMessage::DiagnosticsReport { .. } if cleared => break,
            _ => {}
        }
    }
    bridge.shutdown();
}
