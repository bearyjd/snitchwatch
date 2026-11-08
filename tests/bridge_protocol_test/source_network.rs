use super::*;
use snitchwatch_bridge::rule_policy::SOURCE_NETWORK_REFUSED;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome};
use snitchwatch_proto::protocol::{
    Action, ClientConfig, NotificationReply, NotificationReplyCode, Operator, Rule,
};

/// What turning on a disabled `network` + `source.network` rule did
/// against a daemon reporting `version`: the GUI's result, and the command
/// the daemon got, if any.
async fn turn_on_source_network_rule(
    version: &str,
) -> (
    RuleCommandOutcome,
    Option<snitchwatch_proto::protocol::Notification>,
) {
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
        .unwrap()
        .with_version(version);
    // Subscribe reports the mock's version (`mock-<version>`).
    mock.subscribe_with_config(ClientConfig {
        name: "mock".into(),
        rules: vec![Rule {
            created: 1_800_000_000,
            name: "lan-in".into(),
            enabled: false,
            action: "allow".into(),
            duration: "always".into(),
            operator: Some(Operator {
                r#type: "network".into(),
                operand: "source.network".into(),
                data: "192.168.0.0/16".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    })
    .await
    .unwrap();
    let (reply_tx, mut notifications) = mock.open_notifications().await.unwrap();
    let rules = loop {
        if let ServerMessage::SetRules { rules } = next_server_message(&mut ws).await {
            if !rules.is_empty() {
                break rules;
            }
        }
    };
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|g| *g >= 1))
        .await
        .unwrap()
        .unwrap();
    let listed = rules[0].clone();
    assert!(
        listed["readOnlyReason"].is_null(),
        "an existing source.network rule stays editable: {listed}"
    );
    let mut turned_on = listed;
    turned_on["enabled"] = json!(true);
    let update = ClientMessage::UpdateRule {
        rule_id: "lan-in".into(),
        rule: turned_on,
        request_id: Some("r1".into()),
        reply: None,
    };
    ws.send(Message::Text(serde_json::to_string(&update).unwrap()))
        .await
        .unwrap();

    let sent = tokio::time::timeout(Duration::from_secs(2), notifications.recv())
        .await
        .ok()
        .flatten();
    if let Some(change) = &sent {
        reply_tx
            .send(NotificationReply {
                id: change.id,
                code: NotificationReplyCode::Ok as i32,
                data: String::new(),
            })
            .await
            .unwrap();
    }
    let outcome = loop {
        if let ServerMessage::RuleCommandResult {
            request_id,
            outcome,
        } = next_server_message(&mut ws).await
        {
            assert_eq!(request_id, "r1");
            break outcome;
        }
    };
    bridge.shutdown();
    (outcome, sent)
}

/// opensnitchd 1.9 compiles a `network` condition only with `dest.network`
/// (upstream master `operator.go`): the bridge refuses to turn such a rule
/// on for a daemon that reports 1.9, with words saying what to use instead,
/// and sends nothing.
#[tokio::test]
async fn a_1_9_daemon_gets_no_source_network_rule() {
    let (outcome, sent) = turn_on_source_network_rule("1.9.0").await;
    let RuleCommandOutcome::Refused { problems } = outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].reason, SOURCE_NETWORK_REFUSED);
    assert!(sent.is_none(), "nothing may reach a 1.9 daemon: {sent:?}");
}

/// The shipped daemon is v1.8.0, which compiles it: the same command goes
/// through as before, and the mock at 1.8 accepts what arrives.
#[tokio::test]
async fn a_1_8_daemon_still_gets_its_source_network_rule() {
    let (outcome, sent) = turn_on_source_network_rule("1.8.0").await;
    assert_eq!(outcome, RuleCommandOutcome::Ok);
    let change = sent.expect("the CHANGE_RULE never reached the daemon");
    assert_eq!(change.r#type, Action::ChangeRule as i32);
    assert!(change.rules[0].enabled);
    assert_eq!(
        change.rules[0].operator.as_ref().unwrap().operand,
        "source.network"
    );
    mock_opensnitchd::validate_rule_shape_for(&change.rules[0], "1.8.0")
        .expect("a 1.8 daemon compiles this rule");
    assert!(
        mock_opensnitchd::validate_rule_shape_for(&change.rules[0], "1.9.0").is_err(),
        "the same rule is what a 1.9 daemon refuses"
    );
}
