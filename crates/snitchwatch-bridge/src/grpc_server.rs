//! Bridge-side gRPC server: implements `protocol.UI` and is dialed by
//! opensnitchd as the gRPC client.
//!
//! Replaces the M1 dial-out flow that lived in the now-deleted
//! `grpc_client.rs` and `translator/downstream.rs` envelope hack.

use crate::cache::connections::{ConnectionCache, Verdict};
use crate::notice::NoticeBus;
use crate::translator::connection::{connection_to_row, event_to_row};
use crate::translator::verdict::verdict_to_rule;
use crate::tray_state::{TrayState, TrayStatePublisher};
use crate::ws_messages::{ServerMessage, VerdictDuration};
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

/// How long a `RecentBlock` tray state stays up before reverting to
/// whatever `Idle`/`Pending(n)` the cache actually holds. A UX default with
/// no prior precedent in this codebase to match (long enough for a glance
/// at the tray tooltip, short enough not to hide a still-accurate `Pending`
/// count for long) — easy to tune later, not a measured value.
const RECENT_BLOCK_TTL: Duration = Duration::from_secs(5);

/// Bridge-side gRPC server state. Handed to `UiServer::new` for tonic.
#[derive(Clone)]
pub struct UiService {
    cache: Arc<Mutex<ConnectionCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
    next_ask_id: Arc<AtomicU64>,
    tray_pub: Arc<TrayStatePublisher>,
    notice_bus: Arc<NoticeBus>,
    /// Updated on every `ping()` arrival; read by
    /// `daemon_watchdog::run` (via [`Self::last_ping_handle`]) to detect a
    /// stale/unreachable daemon. `std::sync::Mutex`, not tokio's — this is a
    /// plain timestamp read/write, never held across an `.await`.
    last_ping: Arc<StdMutex<Instant>>,
    /// Guards `RecentBlock`'s revert timer against a race between two
    /// blocks in quick succession: each spawned revert only fires if this
    /// counter still matches the value it captured at spawn time, so an
    /// older block's timer never stomps a newer block's still-live display.
    block_generation: Arc<AtomicU64>,
    /// True while the user has paused interactive filtering (tray
    /// "Pause filtering"). Unlike `last_ping`/`block_generation`, this must
    /// be *writable* from outside `UiService` (the inbound `ClientMessage`
    /// pump toggles it) as well as readable from inside `ask_rule` — the
    /// same shape `tray_pub`/`cache` already have — so it's a genuine
    /// constructor parameter, not internal-only state. See
    /// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`.
    filtering_paused: Arc<AtomicBool>,
}

impl UiService {
    pub fn new(
        cache: Arc<Mutex<ConnectionCache>>,
        broadcast: broadcast::Sender<ServerMessage>,
        tray_pub: Arc<TrayStatePublisher>,
        notice_bus: Arc<NoticeBus>,
        filtering_paused: Arc<AtomicBool>,
    ) -> Self {
        Self {
            cache,
            broadcast,
            next_ask_id: Arc::new(AtomicU64::new(1)),
            tray_pub,
            notice_bus,
            last_ping: Arc::new(StdMutex::new(Instant::now())),
            block_generation: Arc::new(AtomicU64::new(0)),
            filtering_paused,
        }
    }

    /// Convenience: wrap into a tonic `UiServer<UiService>` ready for
    /// `Server::builder().add_service(...)`.
    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }

    /// Handle to the last-ping timestamp, for `daemon_watchdog::run` to
    /// poll. Exposed as an accessor (not a `new()` parameter) so existing
    /// call sites don't need to change.
    pub fn last_ping_handle(&self) -> Arc<StdMutex<Instant>> {
        self.last_ping.clone()
    }

    /// Publish `TrayState::RecentBlock` and schedule its own revert after
    /// [`RECENT_BLOCK_TTL`]. If a second block happens before the first's
    /// timer fires, the first's timer becomes a no-op (its captured
    /// generation no longer matches) — the newer block's own timer owns the
    /// eventual revert, so the tray never flickers back to a stale display
    /// mid-block.
    fn publish_recent_block(&self, what: String) {
        let generation = self.block_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.tray_pub.set(TrayState::RecentBlock {
            what,
            ttl: RECENT_BLOCK_TTL,
        });

        let cache = self.cache.clone();
        let block_generation = self.block_generation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RECENT_BLOCK_TTL).await;
            if block_generation.load(Ordering::SeqCst) == generation {
                // Revert via the cache, which already holds the same
                // publisher and knows the actual current Idle/Pending(n)
                // state — not a hardcoded Idle.
                cache.lock().await.resync_tray_state();
            }
        });
    }
}

#[tonic::async_trait]
impl Ui for UiService {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        let req = request.into_inner();
        let id = req.id;

        // Every ping (not just ones carrying stats) counts as evidence the
        // daemon is alive — see `daemon_watchdog`'s module doc for why this
        // specific handler is the daemon's heartbeat.
        *self.last_ping.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();

        // The daemon's periodic Ping carries `Statistics.events`: recent
        // connections it matched (and decided) against a *pre-existing*
        // rule, entirely without going through the interactive `AskRule`
        // flow above. This is the only place the bridge learns about that
        // traffic and the rule name that governed it — surface each as a
        // decided row so the Connections view's rule-match diagnostics cover
        // every connection, not just the ones the user was prompted for.
        if let Some(stats) = req.stats {
            let new_rows: Vec<_> = stats.events.iter().filter_map(event_to_row).collect();
            if !new_rows.is_empty() {
                {
                    let mut cache = self.cache.lock().await;
                    for row in &new_rows {
                        cache.insert_decided(row.clone());
                    }
                }
                if self.broadcast.receiver_count() > 0 {
                    let msg = ServerMessage::InsertConnectionRows { rows: new_rows };
                    if let Err(e) = self.broadcast.send(msg) {
                        warn!(error = %e, "ping: broadcast send failed");
                    }
                }
            }
        }

        Ok(Response::new(PingReply { id }))
    }

    async fn ask_rule(&self, request: Request<Connection>) -> Result<Response<Rule>, Status> {
        let conn = request.into_inner();
        let ask_id = self.next_ask_id.fetch_add(1, Ordering::Relaxed);

        // Filtering paused (tray "Pause filtering"): auto-allow without
        // prompting. opensnitchd's own DefaultAction stays untouched — only
        // the bridge's own decision policy changes, so a genuine bridge
        // outage (this process crashing, not merely being paused) still
        // hits the daemon's fail-closed default. See
        // docs/superpowers/plans/2026-07-12-tray-filter-off.md.
        if self.filtering_paused.load(Ordering::Relaxed) {
            let row = connection_to_row(&conn, ask_id);
            let mut decided_row = row.clone();
            decided_row.action = Some("allow".to_string());
            {
                let mut cache = self.cache.lock().await;
                cache.insert_decided(decided_row.clone());
            }
            if self.broadcast.receiver_count() > 0 {
                let msg = ServerMessage::InsertConnectionRows {
                    rows: vec![decided_row],
                };
                if let Err(e) = self.broadcast.send(msg) {
                    warn!(error = %e, "ask_rule (paused): broadcast send failed");
                }
            }
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            return Ok(Response::new(verdict_to_rule(
                Verdict::Allow,
                VerdictDuration::Once,
                &conn,
                now_secs,
            )));
        }

        let row = connection_to_row(&conn, ask_id);
        // Captured before `row` moves into the broadcast message below —
        // reused for RecentBlock's tooltip if this resolves to a Deny,
        // rather than re-deriving the same process/host display logic.
        let what = format!("{} → {}", row.process, row.dst_host);
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

        let resolution = verdict_rx
            .await
            .map_err(|_canceled| Status::cancelled("verdict oneshot dropped before resolution"))?;

        if resolution.verdict == Verdict::Deny {
            self.publish_recent_block(what);
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Response::new(verdict_to_rule(
            resolution.verdict,
            resolution.duration,
            &conn,
            now_secs,
        )))
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
        let svc = UiService::new(
            cache,
            tx,
            tray_pub,
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        )
        .into_server();

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
    async fn ping_with_stats_events_inserts_decided_rows_with_matched_rule() {
        use snitchwatch_proto::protocol::{Event, Rule as ProtoRule, Statistics};

        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub,
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        )
        .into_server();
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

        let event = Event {
            time: "2026-07-05T00:00:00Z".to_string(),
            connection: Some(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }),
            rule: Some(ProtoRule {
                created: 1_700_000_000,
                name: "899-curl-allow-out.json".into(),
                description: String::new(),
                enabled: true,
                precedence: false,
                nolog: false,
                action: "allow".into(),
                duration: "always".into(),
                operator: None,
            }),
            unixnano: 1_700_000_000_000_000_000,
        };

        let reply = client
            .ping(PingRequest {
                id: 7,
                stats: Some(Statistics {
                    events: vec![event],
                    ..Default::default()
                }),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(reply.id, 7);

        let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("ping did not broadcast the decided row")
            .expect("broadcast error");
        match broadcasted {
            ServerMessage::InsertConnectionRows { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].dst_host, "example.com");
                assert_eq!(rows[0].action.as_deref(), Some("allow"));
                assert_eq!(
                    rows[0].matched_rule.as_deref(),
                    Some("899-curl-allow-out.json")
                );
            }
            other => panic!("expected InsertConnectionRows, got {other:?}"),
        }
        assert_eq!(cache.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn ping_without_stats_is_a_noop() {
        let addr = spawn_test_service().await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);
        let reply = client
            .ping(PingRequest { id: 3, stats: None })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(reply.id, 3);
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
    use crate::ws_messages::VerdictDuration;

    #[tokio::test]
    async fn ask_rule_blocks_until_cache_resolves_with_allow() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub,
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        )
        .into_server();
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

        cache
            .lock()
            .await
            .resolve(&row_id, Verdict::Allow, VerdictDuration::Once)
            .unwrap();

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
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub,
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        )
        .into_server();
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
            if cache
                .lock()
                .await
                .resolve(&row_id, Verdict::Deny, VerdictDuration::Once)
                .is_ok()
            {
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
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub,
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        )
        .into_server();
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

        let _ = cache
            .lock()
            .await
            .resolve(&ask_row_id(1), Verdict::Deny, VerdictDuration::Once);
        let _ = cache
            .lock()
            .await
            .resolve(&ask_row_id(2), Verdict::Deny, VerdictDuration::Once);
    }

    #[tokio::test(start_paused = true)]
    async fn ask_rule_deny_publishes_recent_block_then_reverts_to_idle() {
        use crate::translator::connection::ask_row_id;
        use crate::ws_messages::VerdictDuration;

        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
            64,
            tray_pub.clone(),
        )));
        let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub.clone(),
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        );

        let mut tray_rx = tray_pub.subscribe();

        let svc_for_ask = svc.clone();
        let ask_handle = tokio::spawn(async move {
            svc_for_ask
                .ask_rule(Request::new(Connection {
                    protocol: "tcp".into(),
                    dst_host: "tracker.example.com".into(),
                    dst_ip: "1.2.3.4".into(),
                    dst_port: 80,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                }))
                .await
        });

        tray_rx.changed().await.unwrap();
        assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

        cache
            .lock()
            .await
            .resolve(&ask_row_id(1), Verdict::Deny, VerdictDuration::Once)
            .unwrap();
        ask_handle.await.unwrap().unwrap();

        tray_rx.changed().await.unwrap();
        match &*tray_rx.borrow() {
            TrayState::RecentBlock { what, .. } => {
                assert!(what.contains("tracker.example.com"), "unexpected: {what}")
            }
            other => panic!("expected RecentBlock, got {other:?}"),
        }

        tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
        tray_rx.changed().await.unwrap();
        assert_eq!(*tray_rx.borrow(), TrayState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn second_deny_within_ttl_supersedes_first_blocks_revert_timer() {
        use crate::translator::connection::ask_row_id;
        use crate::ws_messages::VerdictDuration;

        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
            64,
            tray_pub.clone(),
        )));
        let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub.clone(),
            notice_bus,
            Arc::new(AtomicBool::new(false)),
        );
        let mut tray_rx = tray_pub.subscribe();

        // First block.
        let svc1 = svc.clone();
        let ask1 = tokio::spawn(async move {
            svc1.ask_rule(Request::new(Connection {
                dst_host: "first.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        });
        tray_rx.changed().await.unwrap();
        cache
            .lock()
            .await
            .resolve(&ask_row_id(1), Verdict::Deny, VerdictDuration::Once)
            .unwrap();
        ask1.await.unwrap().unwrap();
        tray_rx.changed().await.unwrap();
        assert!(matches!(&*tray_rx.borrow(), TrayState::RecentBlock { .. }));

        // Halfway through the first block's TTL, a second block supersedes it.
        tokio::time::advance(RECENT_BLOCK_TTL / 2).await;
        let svc2 = svc.clone();
        let ask2 = tokio::spawn(async move {
            svc2.ask_rule(Request::new(Connection {
                dst_host: "second.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        });
        tray_rx.changed().await.unwrap(); // Pending(1) for the second ask
        cache
            .lock()
            .await
            .resolve(&ask_row_id(2), Verdict::Deny, VerdictDuration::Once)
            .unwrap();
        ask2.await.unwrap().unwrap();
        tray_rx.changed().await.unwrap();
        match &*tray_rx.borrow() {
            TrayState::RecentBlock { what, .. } => assert!(what.contains("second.example.com")),
            other => panic!("expected RecentBlock(second), got {other:?}"),
        }

        // When the FIRST block's original TTL would have elapsed, its timer
        // must be a no-op — the tray should still show the second block.
        tokio::time::advance(RECENT_BLOCK_TTL / 2 + Duration::from_millis(50)).await;
        assert!(
            matches!(&*tray_rx.borrow(), TrayState::RecentBlock { what, .. } if what.contains("second.example.com")),
            "first block's timer must not have reverted the tray"
        );

        // Only once the SECOND block's own TTL elapses does it revert.
        tokio::time::advance(RECENT_BLOCK_TTL).await;
        tray_rx.changed().await.unwrap();
        assert_eq!(*tray_rx.borrow(), TrayState::Idle);
    }

    #[tokio::test]
    async fn ask_rule_auto_allows_immediately_when_filtering_paused() {
        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let filtering_paused = Arc::new(AtomicBool::new(true));
        let svc = UiService::new(
            cache.clone(),
            tx,
            tray_pub,
            notice_bus,
            filtering_paused.clone(),
        );

        // No spawn/wait needed: paused ask_rule never blocks on a oneshot.
        let rule = svc
            .ask_rule(Request::new(Connection {
                dst_host: "paused.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(rule.action, "allow");

        // No pending row was ever created.
        assert_eq!(cache.lock().await.pending_count(), 0);
        assert_eq!(cache.lock().await.len(), 1);

        let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("paused ask_rule did not broadcast the decided row")
            .expect("broadcast error");
        match broadcasted {
            ServerMessage::InsertConnectionRows { rows } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].action.as_deref(), Some("allow"));
            }
            other => panic!("expected InsertConnectionRows, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ask_rule_prompts_normally_when_not_paused() {
        use crate::translator::connection::ask_row_id;

        let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
        let notice_bus = Arc::new(crate::notice::NoticeBus::new());
        let filtering_paused = Arc::new(AtomicBool::new(false));
        let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus, filtering_paused);

        let ask_handle = tokio::spawn({
            let svc = svc.clone();
            async move {
                svc.ask_rule(Request::new(Connection {
                    dst_host: "normal.example.com".into(),
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                }))
                .await
            }
        });

        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if cache
                .lock()
                .await
                .resolve(&ask_row_id(1), Verdict::Allow, VerdictDuration::Once)
                .is_ok()
            {
                break;
            }
        }

        let rule = ask_handle.await.unwrap().unwrap().into_inner();
        assert_eq!(rule.action, "allow");
    }
}
