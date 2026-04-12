//! Bridge orchestrator — testable library entry point.
//!
//! This crate's binary (`main.rs`) is a thin wrapper around [`run`]. Tests
//! construct a [`BridgeConfig`] and call `run` directly so they don't have
//! to spawn the CLI as a subprocess.
//!
//! What `run` wires together:
//!
//! 1. Binds the WebSocket server on `ws_bind`.
//! 2. Binds the gRPC `Ui` server on `grpc_bind` — opensnitchd dials in here.
//! 3. Inbound `AskRule` RPCs insert a pending row into the cache, broadcast
//!    it on the WebSocket, and await a `oneshot<Verdict>` from the WS layer.
//! 4. Inbound WebSocket `ClientMessage`s go through `upstream::apply`, which
//!    mutates the cache (resolving pending rows by firing the oneshot).

use anyhow::{Context, Result};
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::translator::upstream;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tonic::transport::Server;
use tracing::{error, info};

/// Runtime configuration for [`run`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Address to bind the gRPC `Ui` server on. opensnitchd will dial this.
    /// Use port `0` for ephemeral.
    pub grpc_bind: SocketAddr,
    /// Address to bind the WebSocket server on. Use port `0` for ephemeral.
    pub ws_bind: SocketAddr,
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

        let ws_bind_str =
            std::env::var("SNITCHWATCH_WS_BIND").unwrap_or_else(|_| "127.0.0.1:0".to_string());
        let ws_bind: SocketAddr = ws_bind_str
            .parse()
            .with_context(|| format!("invalid SNITCHWATCH_WS_BIND: {ws_bind_str}"))?;

        Ok(Self {
            grpc_bind,
            ws_bind,
            cache_capacity: 10_000,
        })
    }
}

/// Handle to a running bridge. Dropping this does **not** shut the bridge
/// down — call [`RunningBridge::shutdown`] explicitly when you're done.
pub struct RunningBridge {
    /// Actual bound WebSocket address.
    pub ws_addr: SocketAddr,
    /// Actual bound gRPC address (so callers who passed `:0` can discover it).
    pub grpc_addr: SocketAddr,
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

    // --- WebSocket server ---------------------------------------------------
    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx,
    };
    let ws_server = WsServer::new(config.ws_bind, ws_handles);
    let (ws_listener, ws_addr) = ws_server
        .bind()
        .await
        .context("failed to bind WebSocket listener")?;
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

    let ui_service = UiService::new(cache.clone(), broadcast_tx.clone()).into_server();
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
        ws_addr,
        grpc_addr,
        ws_shutdown_tx: Some(ws_shutdown_tx),
        grpc_shutdown_tx: Some(grpc_shutdown_tx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_binds_both_ports_and_shutdown_works() {
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_bind: "127.0.0.1:0".parse().unwrap(),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        assert!(bridge.ws_addr.port() != 0);
        assert!(bridge.grpc_addr.port() != 0);
        bridge.shutdown();
    }
}
