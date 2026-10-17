use super::*;

/// Rule enable/disable/delete must actually reach the daemon.
///
/// Before this path existed the bridge produced `UpstreamEffect::UpdateRule`
/// and `DeleteRule` and then dropped them: the outbound `Notifications` stream
/// was parked on `std::future::pending()`, so nothing was ever sent. The Rules
/// page's controls were wired end to end right up to the bridge and stopped
/// there.
///
/// Asserts the daemon-visible result, not just "a notification arrived":
///   * the action is `CHANGE_RULE` / `DELETE_RULE`,
///   * the toggled `enabled` value survives (the entire point of the command),
///   * the rule passes `validate_rule_shape`, which mirrors the daemon's real
///     acceptance path — `Deserialize` *and* `Operator.Compile()`. A rule that
///     fails it is silently discarded by a real daemon, which is what issue #14
///     was, and would make this feature look like it works while doing nothing.
///   * the type is never `NONE`, which would order the daemon to close the
///     stream (`vendor/opensnitch/daemon/ui/notifications.go:405-408`).
#[tokio::test]
async fn rule_update_and_delete_reach_the_daemon_as_notifications() {
    use snitchwatch_bridge::ws_messages::ClientMessage;
    use snitchwatch_proto::protocol::Action;

    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // The stream must be open *before* the effect is sent: the bridge
    // broadcasts to whoever is subscribed and replays nothing.
    let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    // The rule exists on the daemon: a GUI may only change a rule the
    // bridge's list holds (P2.1), and the list comes from this snapshot.
    mock.subscribe_with_config(snitchwatch_proto::protocol::ClientConfig {
        name: "mock".into(),
        rules: vec![snitchwatch_proto::protocol::Rule {
            name: "899-firefox-allow-out".into(),
            enabled: true,
            action: "allow".into(),
            duration: "always".into(),
            operator: Some(snitchwatch_proto::protocol::Operator {
                r#type: "simple".into(),
                operand: "dest.host".into(),
                data: "example.com".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    })
    .await
    .unwrap();
    let (_reply_tx, mut notifications) = mock.open_notifications().await.unwrap();
    // The bridge handles the stream's HELLO asynchronously. A level check,
    // not `changed()`, which would hang if the HELLO was already handled.
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .expect("the daemon stream never said HELLO")
        .unwrap();

    // Shaped exactly like `RulesStore::toggled_rule_json` output: the full
    // rule with `enabled` already flipped to the desired value.
    let toggled = json!({
        "name": "899-firefox-allow-out",
        "enabled": false,
        "action": "allow",
        "duration": "always",
        "description": "",
        "operator": {
            "type": "simple",
            "operand": "dest.host",
            "data": "example.com",
            "sensitive": false,
        },
    });

    bridge
        .inbound_tx
        .send(ClientMessage::UpdateRule {
            rule_id: "899-firefox-allow-out".to_string(),
            rule: toggled,
            request_id: None,
            reply: None,
        })
        .await
        .expect("inbound channel closed");

    let change = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("no CHANGE_RULE notification reached the daemon")
        .expect("notification stream closed");

    assert_eq!(
        change.r#type,
        Action::ChangeRule as i32,
        "rule toggle must arrive as CHANGE_RULE"
    );
    assert_ne!(
        change.r#type,
        Action::None as i32,
        "a NONE-typed notification would close the daemon's stream"
    );
    assert_ne!(change.id, 0, "id 0 collides with the daemon's HELLO reply");
    assert_eq!(change.rules.len(), 1);
    assert_eq!(change.rules[0].name, "899-firefox-allow-out");
    assert!(
        !change.rules[0].enabled,
        "the toggled `enabled` value must survive to the daemon"
    );
    mock_opensnitchd::validate_rule_shape(&change.rules[0])
        .expect("a real daemon would silently reject this rule");

    // Delete needs only the name (notifications.go:132).
    bridge
        .inbound_tx
        .send(ClientMessage::DeleteRule {
            rule_id: "899-firefox-allow-out".to_string(),
            request_id: None,
            reply: None,
        })
        .await
        .expect("inbound channel closed");

    let delete = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("no DELETE_RULE notification reached the daemon")
        .expect("notification stream closed");

    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules.len(), 1);
    assert_eq!(delete.rules[0].name, "899-firefox-allow-out");
    assert!(
        delete.id > change.id,
        "notification ids must increase so daemon replies can be correlated"
    );

    bridge.shutdown();
}

/// Reads WS frames until the next `setRules` and returns its rules.
async fn next_set_rules(ws: &mut WebSocketStream<UnixStream>) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["action"] == "setRules" {
                        return v["rules"].as_array().unwrap().clone();
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for setRules")
}

fn names_and_enabled(rules: &[serde_json::Value]) -> Vec<(String, bool)> {
    rules
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap().to_string(),
                r["enabled"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// Issue #48: the Rules page lists every rule the daemon holds, not only the
/// ones answered this session. The bridge adopts `ClientConfig.rules` from
/// `Subscribe` once the same connection's stream says HELLO, and changes its
/// copy only when the daemon confirms a command with `OK`.
#[tokio::test]
async fn daemon_rules_reach_the_gui_and_follow_confirmed_commands() {
    use snitchwatch_bridge::ws_messages::ClientMessage;
    use snitchwatch_proto::protocol::{
        Action, ClientConfig, NotificationReply, NotificationReplyCode, Operator, Rule,
    };

    let socket_dir = tempfile::tempdir().unwrap();
    let bridge = run(BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 64,
    })
    .await
    .expect("bridge run failed");
    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;
    let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();

    // 1. The daemon subscribes with rules it already holds (stock UI, disk,
    //    an earlier session).
    let daemon_rule = |name: &str| Rule {
        created: 1_800_000_000,
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: "always".to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    mock.subscribe_with_config(ClientConfig {
        name: "mock".into(),
        // `c-stock\ui`: a name the stock UI may save but the bridge never
        // sends back to the daemon (#57); it is listed read-only (#48).
        rules: vec![
            daemon_rule("b-curl"),
            daemon_rule("a-firefox"),
            daemon_rule(r"c-stock\ui"),
        ],
        ..Default::default()
    })
    .await
    .unwrap();

    // 2. No rules yet: nothing is committed before the stream's HELLO, so
    //    the snapshot's setRules is the empty list (#61: it resets a GUI's
    //    stale one). trayState is the last message of a snapshot.
    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let t = match ws.next().await {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            };
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            let listed = v["rules"].as_array().is_some_and(|rules| !rules.is_empty());
            assert!(
                v["action"] != "setRules" || !listed,
                "rules sent before HELLO: {v}"
            );
            if v["action"] == "trayState" {
                break;
            }
        }
    })
    .await
    .expect("snapshot never ended");

    // 3. Opening the stream sends HELLO, which commits the snapshot.
    let (reply_tx, mut notifications) = mock.open_notifications().await.unwrap();
    let rules = next_set_rules(&mut ws).await;
    assert_eq!(
        names_and_enabled(&rules),
        vec![
            ("a-firefox".into(), true),
            ("b-curl".into(), true),
            (r"c-stock\ui".into(), true)
        ]
    );
    assert!(rules[0]["readOnlyReason"].is_null());
    assert!(
        rules[2]["readOnlyReason"].is_string(),
        "an unsendable name must be listed read-only: {}",
        rules[2]
    );

    // 4.
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .unwrap()
        .unwrap();

    // 5. A toggle the daemon accepts is reflected in the full list.
    let mut toggled = rules[0].clone();
    toggled["enabled"] = json!(false);
    bridge
        .inbound_tx
        .send(ClientMessage::UpdateRule {
            rule_id: "a-firefox".into(),
            rule: toggled,
            request_id: None,
            reply: None,
        })
        .await
        .unwrap();
    let change = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(change.r#type, Action::ChangeRule as i32);
    reply_tx
        .send(NotificationReply {
            id: change.id,
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        names_and_enabled(&next_set_rules(&mut ws).await),
        vec![
            ("a-firefox".into(), false),
            ("b-curl".into(), true),
            (r"c-stock\ui".into(), true)
        ]
    );

    // 6. A delete the daemon refuses: opensnitchd dropped the rule from
    //    memory before failing on its file (tower r12), so it leaves the
    //    list.
    bridge
        .inbound_tx
        .send(ClientMessage::DeleteRule {
            rule_id: "b-curl".into(),
            request_id: None,
            reply: None,
        })
        .await
        .unwrap();
    let delete = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    reply_tx
        .send(NotificationReply {
            id: delete.id,
            code: NotificationReplyCode::Error as i32,
            data: "remove b-curl.json: operation not permitted".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        names_and_enabled(&next_set_rules(&mut ws).await),
        vec![("a-firefox".into(), false), (r"c-stock\ui".into(), true)]
    );

    // 7. A malformed toggle is never sent, and the list is re-sent so the
    //    GUI drops its optimistic change.
    bridge
        .inbound_tx
        .send(ClientMessage::UpdateRule {
            rule_id: "b-curl".into(),
            rule: json!({ "name": "b-curl", "enabled": false }),
            request_id: None,
            reply: None,
        })
        .await
        .unwrap();
    assert_eq!(
        names_and_enabled(&next_set_rules(&mut ws).await),
        vec![("a-firefox".into(), false), (r"c-stock\ui".into(), true)]
    );
    assert!(
        notifications.try_recv().is_err(),
        "a malformed rule reached the daemon"
    );

    // 8. The read-only rule: a delete is refused before the daemon, and the
    //    list is re-sent unchanged.
    bridge
        .inbound_tx
        .send(ClientMessage::DeleteRule {
            rule_id: r"c-stock\ui".into(),
            request_id: None,
            reply: None,
        })
        .await
        .unwrap();
    assert_eq!(next_set_rules(&mut ws).await.len(), 2);
    assert!(
        notifications.try_recv().is_err(),
        "a command for a read-only rule reached the daemon"
    );

    bridge.shutdown();
}
