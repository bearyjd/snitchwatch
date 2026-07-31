//! In-process mock of opensnitchd, the gRPC **client** that dials a
//! Snitchwatch bridge.
//!
//! This crate exists for the post-M1.5 topology: the bridge binds the gRPC
//! `Ui` server, and opensnitchd is the client. Tests construct a
//! `MockOpensnitchd::connect(bridge_addr)`, then drive the bridge by calling
//! the same RPCs the real daemon would: `ping`, `subscribe`, `ask_rule`,
//! `notifications`, `post_alert`.

use snitchwatch_proto::protocol::ui_client::UiClient;
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, NotificationReply, PingReply, PingRequest, Rule,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint};

/// The real daemon's `AskRule` deadline
/// (`vendor/opensnitch/daemon/ui/client.go:366`, `context.WithTimeout(...,
/// time.Second*120)`). `ask_rule` applies the same deadline so a bridge that
/// never resolves a pending verdict surfaces as a test failure instead of
/// hanging the test suite indefinitely — see issue #14.
pub const ASK_RULE_DEADLINE: Duration = Duration::from_secs(120);

/// Errors the mock can surface to tests.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    #[error("connect failed: {0}")]
    Connect(#[from] tonic::transport::Error),
    #[error("rpc failed: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("ask_rule timed out after {0:?} (real daemon deadline: vendor/opensnitch/daemon/ui/client.go:366)")]
    AskRuleTimedOut(Duration),
    /// Mirrors `vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize`: the
    /// real daemon rejects a `Rule` shaped like this outright and falls
    /// through to `DefaultAction` rather than applying the verdict. A bridge
    /// that returns a rule shaped like this passes no differently than one
    /// that hangs — both mean the user's verdict was silently discarded.
    #[error("bridge returned a Rule the daemon would reject: {0}")]
    InvalidRule(String),
}

/// Mock opensnitchd as a gRPC client.
#[derive(Clone)]
pub struct MockOpensnitchd {
    client: UiClient<Channel>,
}

impl MockOpensnitchd {
    /// Dial the bridge at `addr`. Caller is responsible for ensuring the
    /// bridge has bound its gRPC port (use `RunningBridge::grpc_addr`).
    pub async fn connect(addr: SocketAddr) -> Result<Self, MockError> {
        let endpoint = Endpoint::from_shared(format!("http://{addr}"))
            .map_err(MockError::Connect)?
            .connect_timeout(Duration::from_secs(2));

        let mut last_err = None;
        for _ in 0..20 {
            match endpoint.connect().await {
                Ok(channel) => {
                    return Ok(Self {
                        client: UiClient::new(channel),
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        Err(MockError::Connect(last_err.unwrap()))
    }

    pub async fn ping(&mut self, id: u64) -> Result<PingReply, MockError> {
        let reply = self
            .client
            .ping(PingRequest { id, stats: None })
            .await?
            .into_inner();
        Ok(reply)
    }

    pub async fn subscribe(&mut self, name: &str) -> Result<ClientConfig, MockError> {
        let cfg = ClientConfig {
            id: 1,
            name: name.to_string(),
            version: "mock-1.6.0".to_string(),
            ..Default::default()
        };
        let echoed = self.client.subscribe(cfg).await?.into_inner();
        Ok(echoed)
    }

    /// Like [`Self::subscribe`], but takes a full `ClientConfig` the
    /// caller controls — used by tests that need to drive a specific
    /// `is_firewall_running`/`config` value through the bridge.
    pub async fn subscribe_with_config(
        &mut self,
        cfg: ClientConfig,
    ) -> Result<ClientConfig, MockError> {
        let echoed = self.client.subscribe(cfg).await?.into_inner();
        Ok(echoed)
    }

    /// Send a single AskRule unary RPC and wait for the bridge's `Rule` reply.
    ///
    /// Applies the real daemon's ~120s deadline ([`ASK_RULE_DEADLINE`]) so a
    /// bridge that never resolves the pending oneshot fails the test loudly
    /// instead of hanging forever, and validates the returned `Rule` the way
    /// `vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize` does — see
    /// issue #14, where a bridge that always sent `operator: None` passed
    /// every mock-driven round-trip test while failing 100% of the time
    /// against a real daemon.
    pub async fn ask_rule(&mut self, conn: Connection) -> Result<Rule, MockError> {
        let rule = tokio::time::timeout(ASK_RULE_DEADLINE, self.client.ask_rule(conn))
            .await
            .map_err(|_elapsed| MockError::AskRuleTimedOut(ASK_RULE_DEADLINE))??
            .into_inner();
        validate_rule_shape(&rule)?;
        Ok(rule)
    }

    pub async fn post_alert(&mut self, alert: Alert) -> Result<MsgResponse, MockError> {
        let reply = self.client.post_alert(alert).await?.into_inner();
        Ok(reply)
    }

    /// Convenience wrapper around [`Self::post_alert`] for the common case
    /// of a text-payload ERROR/WARNING alert (mirrors
    /// `vendor/opensnitch/daemon/ui/alerts.go`'s `NewErrorAlert`/
    /// `NewWarningAlert` shape) — used by tests exercising the daemon-alert
    /// → diagnostics overlay (issue #6) without hand-building an `Alert`.
    pub async fn post_alert_text(
        &mut self,
        r#type: snitchwatch_proto::protocol::alert::Type,
        what: snitchwatch_proto::protocol::alert::What,
        text: &str,
    ) -> Result<MsgResponse, MockError> {
        self.post_alert(Alert {
            id: 1,
            r#type: r#type as i32,
            action: 0,
            priority: 0,
            what: what as i32,
            data: Some(snitchwatch_proto::protocol::alert::Data::Text(
                text.to_string(),
            )),
        })
        .await
    }

    /// Open the bidi `Notifications` stream.
    pub async fn open_notifications(
        &mut self,
    ) -> Result<(mpsc::Sender<NotificationReply>, mpsc::Receiver<u64>), MockError> {
        let (reply_tx, reply_rx) = mpsc::channel::<NotificationReply>(16);
        let outbound = tokio_stream::wrappers::ReceiverStream::new(reply_rx);

        let mut inbound = self.client.notifications(outbound).await?.into_inner();

        let (count_tx, count_rx) = mpsc::channel::<u64>(16);
        tokio::spawn(async move {
            while let Ok(Some(n)) = inbound.message().await {
                if count_tx.send(n.id).await.is_err() {
                    return;
                }
            }
        });

        Ok((reply_tx, count_rx))
    }
}

/// Reject the class of malformed `Rule` the real daemon's `rule.Deserialize`
/// rejects (`vendor/opensnitch/daemon/rule/rule.go:85-89`): `operator: None`
/// ("Deserialize rule, Operator nil" → "invalid operator"), or an operator
/// with an empty `type`/`operand`. A real daemon that rejects the reply
/// falls through to `DefaultAction` instead of applying the verdict — see
/// issue #14.
// `MockError::Rpc(tonic::Status)` is already the large variant driving this;
// the crate's public `MockError`-returning methods (`ask_rule`, `ping`, ...)
// are exempted from this lint as public API, so this private helper needs
// the same allowance rather than boxing just for its own sake.
#[allow(clippy::result_large_err)]
fn validate_rule_shape(rule: &Rule) -> Result<(), MockError> {
    let operator = rule
        .operator
        .as_ref()
        .ok_or_else(|| MockError::InvalidRule("operator is None".to_string()))?;
    if operator.r#type.is_empty() {
        return Err(MockError::InvalidRule("operator.type is empty".to_string()));
    }
    if operator.operand.is_empty() {
        return Err(MockError::InvalidRule(
            "operator.operand is empty".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        let filtering_paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let svc = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused).into_server();
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
}
