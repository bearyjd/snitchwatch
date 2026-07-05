//! Bridge orchestrator — testable library entry point.
//!
//! This crate's binary (`main.rs`) is a thin wrapper around [`run`]. Tests
//! construct a [`BridgeConfig`] and call `run` directly so they don't have
//! to spawn the CLI as a subprocess.
//!
//! What `run` wires together:
//!
//! 1. Binds the WebSocket server on the Unix domain socket at
//!    `ws_socket_path` and writes a fresh handshake token alongside it.
//! 2. Binds the gRPC `Ui` server on `grpc_bind` — opensnitchd dials in here.
//! 3. Inbound `AskRule` RPCs insert a pending row into the cache, broadcast
//!    it on the WebSocket, and await a `oneshot<Verdict>` from the WS layer.
//! 4. Inbound WebSocket `ClientMessage`s go through `upstream::apply`, which
//!    mutates the cache (resolving pending rows by firing the oneshot).

use anyhow::{Context, Result};
use snitchwatch_bridge::auth::{self, Token};
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::BlocklistsManager;
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::cache::traffic_tracker::TrafficTracker;
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::notice::{Notice, NoticeBus};
use snitchwatch_bridge::translator::upstream;
use snitchwatch_bridge::tray_state::{TrayState, TrayStatePublisher};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};
use tonic::transport::Server;
use tracing::{error, info, warn};

/// Rolling window kept by the traffic pump's [`TrafficTracker`], matching
/// `snitchwatch-kirigami::traffic::ring_store::DEFAULT_WINDOW_SECONDS` (the
/// consumer side of the same underlying `TrafficBinner`).
const TRAFFIC_WINDOW_SECONDS: usize = 300;

/// Runtime configuration for [`run`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Address to bind the gRPC `Ui` server on. opensnitchd will dial this.
    /// Use port `0` for ephemeral.
    pub grpc_bind: SocketAddr,
    /// Path to the Unix domain socket the WS server binds. Defaults to
    /// `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock`.
    pub ws_socket_path: PathBuf,
    /// Cache capacity (number of recent rows retained).
    pub cache_capacity: usize,
}

impl BridgeConfig {
    pub fn from_env() -> Result<Self> {
        let grpc_bind_str =
            std::env::var("SNITCHWATCH_GRPC_BIND").unwrap_or_else(|_| "127.0.0.1:0".to_string());
        let grpc_bind: SocketAddr = grpc_bind_str
            .parse()
            .with_context(|| format!("invalid SNITCHWATCH_GRPC_BIND: {grpc_bind_str}"))?;

        let ws_socket_path = std::env::var_os("SNITCHWATCH_WS_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| auth::runtime_dir().join("bridge.sock"));

        Ok(Self {
            grpc_bind,
            ws_socket_path,
            cache_capacity: 10_000,
        })
    }
}

/// Handle to a running bridge. Dropping this does **not** shut the bridge
/// down — call [`RunningBridge::shutdown`] explicitly when you're done.
pub struct RunningBridge {
    /// Unix domain socket path the WS server is listening on.
    pub ws_socket_path: PathBuf,
    /// Path to the token file written alongside the socket (mode 0600).
    pub ws_token_path: PathBuf,
    /// The handshake token itself, so in-process callers (e.g. the Tauri
    /// shell, tests) don't have to re-read it from disk.
    pub ws_token: Token,
    /// Actual bound gRPC address (so callers who passed `:0` can discover it).
    pub grpc_addr: SocketAddr,
    /// Outbound `ServerMessage` broadcast sender. In-process consumers (the
    /// native Kirigami shell) call `.subscribe()` here to receive the exact
    /// stream the WebSocket server fans out to browser clients — no WS
    /// round-trip to ourselves. The WS server keeps using its own clone of this
    /// same sender, so both consumption paths stay in lockstep.
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
    /// Inbound `ClientMessage` sender. In-process consumers push UI-origin
    /// messages here — the same channel the WebSocket server feeds — so they
    /// flow through the identical `upstream::apply` pump (verdict resolution,
    /// rule effects). This is the in-process equivalent of a WS client frame.
    pub inbound_tx: mpsc::Sender<ClientMessage>,
    /// Receiver for tray icon state changes published by the bridge.
    pub tray_rx: watch::Receiver<TrayState>,
    /// Receiver for desktop notifications published by the bridge.
    pub notice_rx: broadcast::Receiver<Notice>,
    ws_shutdown_tx: Option<oneshot::Sender<()>>,
    grpc_shutdown_tx: Option<oneshot::Sender<()>>,
}

impl RunningBridge {
    /// Signal every background task to stop. Safe to call more than once.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.ws_shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.grpc_shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Start the bridge and return as soon as every background task is running.
///
/// Both the WebSocket server and the gRPC `Ui` server are bound and accepting
/// connections by the time this returns. opensnitchd can dial in immediately.
pub async fn run(config: BridgeConfig) -> Result<RunningBridge> {
    info!(?config, "starting snitchwatch-bridge");

    // Channels between the WS server and the orchestrator.
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(256);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(256);

    // Shared connection cache (pending-row state + decided-row history).
    let cache = Arc::new(Mutex::new(ConnectionCache::new(config.cache_capacity)));

    // --- BlocklistsManager (in-memory store; callers may swap in a persisted one) ---
    let blocklists_store = Arc::new(
        BlocklistStore::open_in_memory().context("failed to open in-memory blocklist store")?,
    );
    let blocklists_mgr = Arc::new(BlocklistsManager::new(blocklists_store));

    // --- WebSocket server ---------------------------------------------------
    // Generate a fresh handshake token and write it to a file alongside the
    // socket (see `snitchwatch_bridge::auth` for why this is a file, not an
    // env var: a Flatpak-sandboxed GUI client won't share this process's
    // environment, but can read a file under the same
    // `$XDG_RUNTIME_DIR/snitchwatch/` the socket lives under).
    let token = Token::generate();
    let ws_token_path = config
        .ws_socket_path
        .parent()
        .map(|p| p.join("token"))
        .unwrap_or_else(|| PathBuf::from("token"));
    auth::write_token_file(&token, &ws_token_path).context("failed to write token file")?;

    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx.clone(),
        blocklists: blocklists_mgr,
    };
    let ws_server = WsServer::new(config.ws_socket_path.clone(), token.clone(), ws_handles);
    let ws_listener = ws_server
        .bind()
        .await
        .context("failed to bind WebSocket unix socket")?;
    let (ws_shutdown_tx, ws_shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            res = ws_server.serve(ws_listener) => {
                if let Err(e) = res {
                    error!(error = %e, "ws_server::serve exited");
                }
            }
            _ = ws_shutdown_rx => {
                info!("ws server shutdown signal received");
            }
        }
    });

    // --- gRPC Ui server -----------------------------------------------------
    let grpc_listener = tokio::net::TcpListener::bind(config.grpc_bind)
        .await
        .with_context(|| format!("failed to bind gRPC listener on {}", config.grpc_bind))?;
    let grpc_addr = grpc_listener
        .local_addr()
        .context("gRPC listener has no local address")?;

    let tray_pub = Arc::new(TrayStatePublisher::new());
    let notice_bus = Arc::new(NoticeBus::new());
    let tray_rx = tray_pub.subscribe();
    let notice_rx = notice_bus.subscribe();

    let ui_service =
        UiService::new(cache.clone(), broadcast_tx.clone(), tray_pub, notice_bus).into_server();
    let (grpc_shutdown_tx, grpc_shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
        let serve = Server::builder()
            .add_service(ui_service)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = grpc_shutdown_rx.await;
            });
        if let Err(e) = serve.await {
            error!(error = %e, "grpc Ui server exited");
        } else {
            info!("grpc Ui server shutdown signal received");
        }
    });

    // --- Upstream pump: WS client messages → cache (→ oneshot resolve) -------
    let cache_for_upstream = cache.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let mut cache = cache_for_upstream.lock().await;
            match upstream::apply(&mut cache, msg) {
                Ok(effect) => info!(?effect, "applied upstream effect"),
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    // --- Traffic pump: connection-row byte counters → binned TrafficEvents --
    // Additive: subscribes to the same outbound broadcast every other
    // consumer uses and folds each connection-row batch's byte counters
    // through `TrafficTracker` (wrapping the existing, already-tested
    // `TrafficBinner`), re-broadcasting the result as `TrafficEvents` — the
    // one typed traffic variant the native Kirigami shell's `TrafficModel`
    // consumes (`bridge_dispatch::interests_traffic`). Never touches the
    // legacy `SetTrafficData`/`UpdateTrafficData` variants.
    let mut traffic_rx = broadcast_tx.subscribe();
    let traffic_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        let mut tracker = TrafficTracker::new(TRAFFIC_WINDOW_SECONDS);
        loop {
            let msg = match traffic_rx.recv().await {
                Ok(msg) => msg,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "traffic pump lagged behind broadcast");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let rows = match &msg {
                ServerMessage::InsertConnectionRows { rows } => rows,
                ServerMessage::UpdateConnectionRows { rows } => rows,
                _ => continue,
            };
            if rows.is_empty() {
                continue;
            }
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let events = tracker.record_rows(now_ms, rows);
            if traffic_tx.receiver_count() > 0 {
                if let Err(e) = traffic_tx.send(ServerMessage::TrafficEvents { events }) {
                    warn!(error = %e, "traffic pump: broadcast send failed");
                }
            }
        }
    });

    Ok(RunningBridge {
        ws_socket_path: config.ws_socket_path,
        ws_token_path,
        ws_token: token,
        grpc_addr,
        broadcast_tx,
        inbound_tx,
        tray_rx,
        notice_rx,
        ws_shutdown_tx: Some(ws_shutdown_tx),
        grpc_shutdown_tx: Some(grpc_shutdown_tx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_binds_socket_and_grpc_port_and_shutdown_works() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        assert!(bridge.ws_socket_path.exists());
        assert!(bridge.ws_token_path.exists());
        assert!(bridge.grpc_addr.port() != 0);
        bridge.shutdown();
    }

    #[tokio::test]
    async fn exposes_in_process_broadcast_and_inbound_handles() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");

        // Outbound: a subscriber gets the exact ServerMessage the bridge fans out.
        let mut rx = bridge.broadcast_tx.subscribe();
        let msg = ServerMessage::ClearConnectionRows;
        bridge.broadcast_tx.send(msg.clone()).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("no broadcast within timeout")
            .expect("broadcast channel closed");
        assert_eq!(got, msg);

        // Inbound: a UI-origin ClientMessage is accepted onto the upstream pump.
        bridge
            .inbound_tx
            .send(ClientMessage::Undo)
            .await
            .expect("inbound channel closed");

        bridge.shutdown();
    }

    #[tokio::test]
    async fn synthetic_connection_activity_is_rebroadcast_as_traffic_events() {
        use snitchwatch_bridge::ws_messages::ConnectionRow;

        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        let mut rx = bridge.broadcast_tx.subscribe();

        // Simulate what `UiService::ask_rule` broadcasts on a real connection
        // (a synthetic row with non-zero byte counters, since production
        // `ask_rule` rows start at zero — this exercises the pump's mapping
        // end-to-end regardless of what today's actual producer sends).
        let row = ConnectionRow {
            id: "ask-1".into(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 1234,
            bytes_received: 5678,
            started_at_ms: 0,
        };
        bridge
            .broadcast_tx
            .send(ServerMessage::InsertConnectionRows {
                rows: vec![row.clone()],
            })
            .expect("broadcast send failed");

        // First: the original InsertConnectionRows, echoed to every subscriber
        // (including this test's own, exactly like a browser WS client).
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("no broadcast within timeout")
            .expect("broadcast channel closed");
        assert_eq!(
            first,
            ServerMessage::InsertConnectionRows { rows: vec![row] }
        );

        // Second: the traffic pump's derived TrafficEvents, mapping
        // bytes_sent -> bytesOut and bytes_received -> bytesIn.
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("no TrafficEvents broadcast within timeout")
            .expect("broadcast channel closed");
        match second {
            ServerMessage::TrafficEvents { events } => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].bytes_in, 5678);
                assert_eq!(events[0].bytes_out, 1234);
            }
            other => panic!("expected TrafficEvents, got {other:?}"),
        }

        bridge.shutdown();
    }
}
