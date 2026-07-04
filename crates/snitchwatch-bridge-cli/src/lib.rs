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
use tracing::{error, info};

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
        inbound: inbound_tx,
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

    Ok(RunningBridge {
        ws_socket_path: config.ws_socket_path,
        ws_token_path,
        ws_token: token,
        grpc_addr,
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
}
