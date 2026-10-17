mod rule_shape;

use super::*;
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::ws_messages::ServerMessage;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tonic::transport::Server;

async fn spawn_bridge_grpc() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let tray_pub = Arc::new(snitchwatch_bridge::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(snitchwatch_bridge::notice::NoticeBus::new());
    let filter_pause = Arc::new(snitchwatch_bridge::filter_pause::FilterPause::new());
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filter_pause).into_server();
    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn mock_can_ping_bridge() {
    let addr = spawn_bridge_grpc().await;
    let mut mock = MockOpensnitchd::connect(addr).await.unwrap();
    let reply = mock.ping(123).await.unwrap();
    assert_eq!(reply.id, 123);
}

#[tokio::test]
async fn mock_can_subscribe_to_bridge() {
    let addr = spawn_bridge_grpc().await;
    let mut mock = MockOpensnitchd::connect(addr).await.unwrap();
    let echoed = mock.subscribe("opensnitchd-mock").await.unwrap();
    assert_eq!(echoed.name, "opensnitchd-mock");
}

#[test]
fn ask_rule_deadline_returns_the_real_value_or_less() {
    // Doesn't touch env state (see `ask_rule_deadline`'s doc comment on
    // why: `OnceLock`-cached, and `set_var`/`remove_var` are unsound to
    // call from a multi-threaded test binary). Whatever this process's
    // env happened to have at first call, the deadline can never exceed
    // the real value.
    assert!(ask_rule_deadline() <= ASK_RULE_DEADLINE);
}

// -- clamp_deadline_override / FIX MEDIUM-3 (issue #14 security review) --

#[test]
fn deadline_override_defaults_to_real_value_when_unset() {
    assert_eq!(clamp_deadline_override(None), ASK_RULE_DEADLINE);
}

#[test]
fn deadline_override_shrinks_when_smaller() {
    assert_eq!(
        clamp_deadline_override(Some("500")),
        Duration::from_millis(500)
    );
}

#[test]
fn deadline_override_cannot_exceed_the_real_deadline() {
    // u64::MAX ms is ~584 million years — must clamp down to the real
    // 120s value, not disable the deadline.
    assert_eq!(
        clamp_deadline_override(Some(&u64::MAX.to_string())),
        ASK_RULE_DEADLINE
    );
}

#[test]
fn deadline_override_ignores_unparseable_values() {
    assert_eq!(
        clamp_deadline_override(Some("not-a-number")),
        ASK_RULE_DEADLINE
    );
}

// -- E3: the fork's default-applied event ------------------------------

#[test]
fn a_default_action_event_has_the_agreed_shape() {
    let conn = Connection {
        dst_host: "example.com".into(),
        ..Default::default()
    };
    let event = default_action_event(conn.clone(), "reject", 1_700_000_000_123_456_789);
    assert_eq!(event.connection, Some(conn));
    assert_eq!(event.unixnano, 1_700_000_000_123_456_789);
    let rule = event.rule.unwrap();
    assert_eq!(rule.name, "");
    assert_eq!(rule.description, "snitchwatch:default-action");
    assert_eq!(rule.action, "reject");
    assert_eq!(rule.duration, "once");
    assert!(rule.enabled && !rule.precedence && !rule.nolog);
    assert_eq!(rule.created, 1_700_000_000);
    let op = rule.operator.as_ref().unwrap();
    assert_eq!(
        (op.r#type.as_str(), op.operand.as_str(), op.data.as_str()),
        ("simple", "true", "")
    );
    assert!(!op.sensitive && op.list.is_empty());
    // The bridge recognizes it as exactly that.
    assert_eq!(
        DEFAULT_ACTION_MARKER,
        snitchwatch_bridge::daemon_contract::DEFAULT_ACTION_MARKER
    );
    assert!(snitchwatch_bridge::daemon_contract::is_default_action_rule(
        &rule
    ));
}
