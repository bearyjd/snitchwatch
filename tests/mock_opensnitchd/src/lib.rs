//! In-process mock of opensnitchd's gRPC server.
//!
//! The mock implements the `UI` trait from the generated proto stubs and
//! takes scripted event sequences via a public API. Tests construct a mock,
//! script some events, hand the gRPC server's address to the bridge under
//! test, and assert on the resulting WebSocket message stream.
//!
//! The proto package is `protocol`, so the generated server trait lives at
//! `snitchwatch_proto::protocol::ui_server::Ui`.

use snitchwatch_proto::protocol::{
    ui_server::{Ui, UiServer},
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{transport::Server, Request, Response, Status, Streaming};

/// Scripted event the mock can deliver to the bridge.
#[derive(Debug, Clone)]
pub enum ScriptedEvent {
    /// Send a Notification with the given payload to the connected client.
    Notification(Notification),
    /// Wait this many milliseconds before delivering the next event.
    Delay(u64),
    /// Forcibly close the stream (simulates daemon crash / network drop).
    Disconnect,
}

#[derive(Default)]
pub struct MockState {
    pub scripted: Vec<ScriptedEvent>,
    pub received_replies: Vec<NotificationReply>,
    pub ask_rule_default: Option<Rule>,
}

#[derive(Clone, Default)]
pub struct MockOpensnitchd {
    state: Arc<Mutex<MockState>>,
}

impl MockOpensnitchd {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn script(&self, events: Vec<ScriptedEvent>) {
        self.state.lock().await.scripted = events;
    }

    pub async fn set_ask_rule_default(&self, rule: Rule) {
        self.state.lock().await.ask_rule_default = Some(rule);
    }

    pub async fn received_replies(&self) -> Vec<NotificationReply> {
        self.state.lock().await.received_replies.clone()
    }

    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }

    /// Spawn the mock on a random local port and return its address.
    pub async fn spawn(self) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = self.into_server();

        tokio::spawn(async move {
            Server::builder()
                .add_service(server)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });

        // Brief pause to let the server actually start accepting connections.
        tokio::time::sleep(Duration::from_millis(50)).await;
        addr
    }
}

#[tonic::async_trait]
impl Ui for MockOpensnitchd {
    type NotificationsStream =
        Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send + 'static>>;

    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        Ok(Response::new(PingReply::default()))
    }

    async fn ask_rule(&self, _request: Request<Connection>) -> Result<Response<Rule>, Status> {
        let default = self.state.lock().await.ask_rule_default.clone();
        Ok(Response::new(default.unwrap_or_default()))
    }

    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        Ok(Response::new(request.into_inner()))
    }

    async fn post_alert(&self, _request: Request<Alert>) -> Result<Response<MsgResponse>, Status> {
        Ok(Response::new(MsgResponse::default()))
    }

    async fn notifications(
        &self,
        request: Request<Streaming<NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        let state = self.state.clone();
        let mut inbound = request.into_inner();

        // Collector: record every NotificationReply the bridge sends.
        let collector_state = state.clone();
        tokio::spawn(async move {
            while let Ok(Some(reply)) = inbound.message().await {
                collector_state.lock().await.received_replies.push(reply);
            }
        });

        // Outbound: drain scripted events onto an mpsc channel.
        let (tx, rx) = mpsc::channel::<Result<Notification, Status>>(16);
        let script = state.lock().await.scripted.clone();

        tokio::spawn(async move {
            for event in script {
                match event {
                    ScriptedEvent::Notification(n) => {
                        if tx.send(Ok(n)).await.is_err() {
                            return;
                        }
                    }
                    ScriptedEvent::Delay(ms) => {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                    ScriptedEvent::Disconnect => {
                        // Drop tx to close the stream.
                        return;
                    }
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream) as Self::NotificationsStream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_spawns_and_accepts_ping() {
        let mock = MockOpensnitchd::new();
        let addr = mock.spawn().await;

        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = snitchwatch_proto::protocol::ui_client::UiClient::new(channel);
        let reply = client.ping(PingRequest::default()).await.unwrap();
        let _ = reply.into_inner();
    }

    #[tokio::test]
    async fn script_stores_events() {
        let mock = MockOpensnitchd::new();
        mock.script(vec![ScriptedEvent::Delay(10), ScriptedEvent::Disconnect])
            .await;
        let state = mock.state.lock().await;
        assert_eq!(state.scripted.len(), 2);
    }
}
