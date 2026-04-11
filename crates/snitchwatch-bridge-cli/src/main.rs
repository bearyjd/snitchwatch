//! Bridge CLI — runs the bridge against either real opensnitchd or the mock.
//!
//! Usage:
//!   snitchwatch-bridge-cli
//!
//! Env vars (all optional):
//!   SNITCHWATCH_GRPC      gRPC endpoint (default: http://127.0.0.1:50051)
//!   SNITCHWATCH_WS_BIND   WebSocket bind address (default: 127.0.0.1:0)
//!
//! On startup the CLI prints `WS_LISTEN_ADDR=<addr>` to stdout so tests and
//! wrapping processes can discover the port.

use anyhow::Result;
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::grpc_client::GrpcClient;
use snitchwatch_bridge::translator::upstream;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let grpc_url = std::env::var("SNITCHWATCH_GRPC")
        .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string());
    let ws_bind = std::env::var("SNITCHWATCH_WS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_string());

    info!(%grpc_url, %ws_bind, "starting snitchwatch-bridge-cli");

    // Channels connecting the WS server to the orchestrator:
    //   broadcast_tx → fanout of ServerMessages to every connected WS client
    //   inbound_rx   → stream of ClientMessages from any WS client
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(256);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(256);

    // Shared connection cache (pending-row state + decided-row history)
    let cache = Arc::new(Mutex::new(ConnectionCache::new(10_000)));

    // Bring the WS server up first so tests can dial us before the gRPC
    // client has finished its backoff dance.
    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx,
    };
    let ws_server = WsServer::new(ws_bind.parse()?, ws_handles);
    let (listener, ws_addr) = ws_server.bind().await?;
    println!("WS_LISTEN_ADDR={}", ws_addr); // machine-parseable for tests
    tokio::spawn(async move {
        if let Err(e) = ws_server.serve(listener).await {
            error!(error = %e, "ws_server::serve exited");
        }
    });

    // gRPC client — connect (with backoff) to opensnitchd or the mock.
    let client = GrpcClient::new(grpc_url);
    let _channel = client.connect_with_backoff().await?;

    // TODO (Task 20): subscribe to the daemon's notification stream and pump
    // events through `downstream::translate_notification` into `broadcast_tx`.
    // The exact subscription RPC depends on the proto.

    // Inbound loop: drain WS client messages and apply them via the router.
    let cache_for_inbound = cache.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let mut cache = cache_for_inbound.lock().await;
            match upstream::apply(&mut cache, msg) {
                Ok(effect) => {
                    info!(?effect, "applied upstream effect");
                    // TODO (Task 20): side effects that talk to gRPC (AddRule,
                    // DeleteRule, UpdateRule, verdict replies).
                }
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    // Park forever (or until Ctrl-C).
    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received");
    Ok(())
}
