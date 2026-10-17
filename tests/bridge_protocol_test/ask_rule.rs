use super::*;

#[tokio::test]
async fn ask_rule_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Boot the bridge: ephemeral gRPC port + a Unix socket under a fresh
    //    temp dir for the WS server.
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // 2. Connect a WebSocket client (presenting the handshake token) BEFORE
    //    the AskRule call so we don't miss the broadcast.
    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    // 3. Spawn an opensnitchd mock client and fire AskRule in the background.
    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "tcp".into(),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    // 4. Wait for the InsertConnectionRows broadcast on the WS.
    let insert_msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value =
                        serde_json::from_str(&t).expect("server sent bad json");
                    if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                        break v;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for insertConnectionRows");

    let rows = insert_msg
        .get("rows")
        .and_then(|r| r.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let row_id = row
        .get("id")
        .and_then(|v| v.as_str())
        .expect("row id")
        .to_string();
    assert_eq!(row.get("process").and_then(|v| v.as_str()), Some("curl"));
    assert_eq!(
        row.get("dstHost").and_then(|v| v.as_str()),
        Some("example.com")
    );
    assert_eq!(row.get("dstPort").and_then(|v| v.as_u64()), Some(443));
    assert!(
        row.get("action").map(|v| v.is_null()).unwrap_or(false),
        "pending rows must have action: null"
    );

    // 5. Send a SetVerdict to decide the pending row.
    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "allow",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string()))
        .await
        .expect("ws send failed");

    // 6. The mock's AskRule call should now return with action=allow.
    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .expect("ask_rule timed out")
        .expect("ask_rule task panicked");
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(!rule.name.is_empty(), "daemon rejects empty rule names");

    bridge.shutdown();
}

#[tokio::test]
async fn deny_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "udp".into(),
            dst_host: "tracker.bad".into(),
            dst_ip: "1.2.3.4".into(),
            dst_port: 53,
            process_path: "/usr/bin/dnsmasq".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    // Drain WS until we see the insert.
    let row_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                    return v["rows"][0]["id"].as_str().unwrap().to_string();
                }
            }
        }
    })
    .await
    .unwrap();

    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "deny",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string())).await.unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "deny");

    bridge.shutdown();
}

/// Plan `2026-10-08-inline-deny-until-restart.md`: the Kirigami shell's
/// inline Deny on a row with an absolute program path, end to end. The GUI
/// sends exactly the JSON `pending_decision::build_verdict_message` builds
/// for it (pinned on the Kirigami side by `inline_deny.rs`'s
/// `inline_tokens_reach_the_wire_as_until_restart_or_once`), and the daemon
/// gets a rule it stores until it restarts, bound to the program and host, so
/// the kernel's SYN retransmit is refused instead of asked about again.
#[tokio::test]
async fn ask_rule_round_trip_inline_deny_until_restart() {
    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    })
    .await
    .expect("bridge run failed");
    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    // A synced rule list, as the daemon sends before it asks: a remembered
    // answer is announced only to one (PR #106 review H1).
    let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
    mock.subscribe("mock").await.unwrap();
    let (_reply_tx, _notifications) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .expect("the daemon stream never said HELLO")
        .unwrap();
    let ask = tokio::spawn(async move {
        mock.ask_rule(Connection {
            protocol: "tcp".into(),
            dst_host: "github.com".into(),
            dst_ip: "140.82.112.3".into(),
            dst_port: 443,
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    let row_id = loop {
        if let ServerMessage::InsertConnectionRows { rows } = next_server_message(&mut ws).await {
            break rows[0].id.clone();
        }
    };
    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "deny",
        "scope": "this_host",
        "duration": "until_restart",
    });
    ws.send(Message::Text(verdict.to_string())).await.unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask)
        .await
        .expect("ask_rule timed out")
        .expect("ask_rule task panicked");
    assert_eq!(
        (rule.action.as_str(), rule.duration.as_str()),
        ("deny", "until restart")
    );
    let op = rule.operator.as_ref().expect("operator");
    assert_eq!(op.r#type, "list", "{op:?}");
    let operands: Vec<_> = op
        .list
        .iter()
        .map(|o| (o.operand.as_str(), o.data.as_str(), o.sensitive))
        .collect();
    assert_eq!(
        operands,
        vec![
            ("process.path", "/usr/bin/curl", true),
            ("dest.host", "github.com", false),
        ]
    );
    mock_opensnitchd::validate_rule_shape(&rule).expect("a real daemon would reject this rule");

    let announced = loop {
        if let ServerMessage::UpdateRules { rules } = next_server_message(&mut ws).await {
            break rules;
        }
    };
    assert_eq!(announced.len(), 1, "{announced:?}");
    assert_eq!(announced[0]["name"], rule.name);
    assert_eq!(announced[0]["duration"], "until restart");

    bridge.shutdown();
}
