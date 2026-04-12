//! Bridge-side gRPC server: implements `protocol.UI` and is dialed by
//! opensnitchd as the gRPC client.
//!
//! Replaces the M1 dial-out flow that lived in the now-deleted
//! `grpc_client.rs` and `translator/downstream.rs` envelope hack.

use crate::cache::connections::ConnectionCache;
use crate::notice::NoticeBus;
use crate::translator::connection::connection_to_row;
use crate::translator::verdict::verdict_to_rule;
use crate::tray_state::TrayStatePublisher;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

/// Bridge-side gRPC server state. Handed to `UiServer::new` for tonic.
#[derive(Clone)]
pub struct UiService {
    cache: Arc<Mutex<ConnectionCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
    next_ask_id: Arc<AtomicU64>,
    // Stored for future use by M3+ gRPC handlers; not yet read.
    #[allow(dead_code)]
    tray_pub: Arc<TrayStatePublisher>,
    notice_bus: Arc<NoticeBus>,
}

impl UiService {
    pub fn new(
        cache: Arc<Mutex<ConnectionCache>>,
        broadcast: broadcast::Sender<ServerMessage>,
        tray_pub: Arc<TrayStatePublisher>,
        notice_bus: Arc<NoticeBus>,
    ) -> Self {
        Self {
            cache,
            broadcast,
            next_ask_id: Arc::new(AtomicU64::new(1)),
            tray_pub,
            notice_bus,
        }
    }

    /// Convenience: wrap into a tonic `UiServer<UiService>` ready for
    /// `Server::builder().add_service(...)`.
    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }
}

#[tonic::async_trait]
impl Ui for UiService {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        let id = request.into_inner().id;
        Ok(Response::new(PingReply { id }))
    }

    async fn ask_rule(&self, request: Request<Connection>) -> Result<Response<Rule>, Status> {
        let conn = request.into_inner();
        let ask_id = self.next_ask_id.fetch_add(1, Ordering::Relaxed);

        let row = connection_to_row(&conn, ask_id);
        let verdict_rx = {
            let mut cache = self.cache.lock().await;
            cache.insert_pending(row.clone())
        };

        if self.broadcast.receiver_count() > 0 {
            let msg = ServerMessage::InsertConnectionRows { rows: vec![row] };
            if let Err(e) = self.broadcast.send(msg) {
                warn!(error = %e, "broadcast send failed");
            }
        }

        // Notify desktop (Tauri shell shows a notification bubble).
        self.notice_bus.send(crate::notice::Notice::Pending {
            row_id: ask_id,
            process: conn.process_path.clone(),
        });

        let verdict = verdict_rx
            .await
            .map_err(|_canceled| Status::cancelled("verdict oneshot dropped before resolution"))?;

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Response::new(verdict_to_rule(verdict, &conn, now_secs)))
    }

    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        let cfg = request.into_inner();
        info!(client = %cfg.name, version = %cfg.version, "client subscribed");
        Ok(Response::new(cfg))
    }

    async fn post_alert(&self, request: Request<Alert>) -> Result<Response<MsgResponse>, Status> {
        let alert = request.into_inner();
        info!(id = alert.id, type_ = alert.r#type, "alert received");
        Ok(Response::new(MsgResponse { id: alert.id }))
    }

    type NotificationsStream =
        Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send + 'static>>;

    async fn notifications(
        &self,
        request: Request<Streaming<NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        info!("notifications stream opened");
        let mut inbound = request.into_inner();
        tokio::spawn(async move {
            while let Ok(Some(reply)) = inbound.message().await {
                info!(
                    id = reply.id,
                    code = reply.code,
                    "notification reply from daemon"
                );
            }
            warn!("notification reply stream ended");
        });

        let outbound = async_stream::try_stream! {
            // Hold the stream open with no commands until M3+ wires up
            // config-push from the GUI side.
            let () = std::future::pending().await;
            yield Notification::default();
        };

        Ok(Response::new(
            Box::pin(outbound) as Self::NotificationsStream
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_proto::protocol::ui_client::UiClient;
    use std::time::Duration;
    use tokio::sync::{broadcast, Mutex};
    use tonic::transport::Server;

    async fn spawn_test_service() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, _rx) = broadcast::channel(16);
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(cache, tx, tray_pub, notice_bus).into_server();

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
    async fn ping_round_trips_id() {
        let addr = spawn_test_service().await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);
        let reply = client
            .ping(PingRequest {
                id: 99,
                stats: None,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(reply.id, 99);
    }

    #[tokio::test]
    async fn subscribe_echoes_config() {
        let addr = spawn_test_service().await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);
        let cfg = ClientConfig {
            id: 1,
            name: "opensnitchd-test".to_string(),
            version: "1.6.0".to_string(),
            ..Default::default()
        };
        let echoed = client.subscribe(cfg.clone()).await.unwrap().into_inner();
        assert_eq!(echoed.name, cfg.name);
        assert_eq!(echoed.version, cfg.version);
    }

    use crate::cache::connections::Verdict;
    use crate::translator::connection::ask_row_id;

    #[tokio::test]
    async fn ask_rule_blocks_until_cache_resolves_with_allow() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);

        let ask_handle = tokio::spawn(async move {
            client
                .ask_rule(Connection {
                    protocol: "tcp".into(),
                    dst_host: "example.com".into(),
                    dst_ip: "93.184.216.34".into(),
                    dst_port: 443,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                })
                .await
        });

        let inserted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("ask_rule did not broadcast")
            .expect("broadcast error");
        let row_id = match inserted {
            ServerMessage::InsertConnectionRows { rows } => rows[0].id.clone(),
            other => panic!("expected InsertConnectionRows, got {other:?}"),
        };
        assert_eq!(row_id, ask_row_id(1));

        cache.lock().await.resolve(&row_id, Verdict::Allow).unwrap();

        let rule = ask_handle.await.unwrap().unwrap().into_inner();
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.duration, "once");
        assert!(!rule.name.is_empty());
    }

    #[tokio::test]
    async fn ask_rule_returns_deny_rule_when_resolved_with_deny() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, _rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);

        let ask_handle = tokio::spawn(async move {
            client
                .ask_rule(Connection {
                    protocol: "tcp".into(),
                    dst_host: "tracker.example.com".into(),
                    dst_ip: "1.2.3.4".into(),
                    dst_port: 80,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                })
                .await
        });

        let row_id = ask_row_id(1);
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if cache.lock().await.resolve(&row_id, Verdict::Deny).is_ok() {
                break;
            }
        }

        let rule = ask_handle.await.unwrap().unwrap().into_inner();
        assert_eq!(rule.action, "deny");
    }

    #[tokio::test]
    async fn two_concurrent_ask_rules_get_distinct_ask_ids() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        for _ in 0..2 {
            let endpoint = format!("http://{addr}");
            tokio::spawn(async move {
                let channel = tonic::transport::Endpoint::from_shared(endpoint)
                    .unwrap()
                    .connect()
                    .await
                    .unwrap();
                let mut client = UiClient::new(channel);
                let _ = client.ask_rule(Connection::default()).await;
            });
        }

        let mut seen = std::collections::HashSet::new();
        while seen.len() < 2 {
            let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("missed broadcast")
                .expect("broadcast error");
            if let ServerMessage::InsertConnectionRows { rows } = msg {
                for r in rows {
                    seen.insert(r.id);
                }
            }
        }
        assert!(seen.contains(&ask_row_id(1)));
        assert!(seen.contains(&ask_row_id(2)));

        let _ = cache.lock().await.resolve(&ask_row_id(1), Verdict::Deny);
        let _ = cache.lock().await.resolve(&ask_row_id(2), Verdict::Deny);
    }
}
