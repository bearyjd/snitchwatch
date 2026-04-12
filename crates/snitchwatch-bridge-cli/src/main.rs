//! Bridge CLI — runs the bridge that exposes a gRPC `Ui` server (which
//! opensnitchd dials in to) and a WebSocket server for the GUI front-end.
//!
//! Usage:
//!   snitchwatch-bridge-cli
//!
//! Env vars (all optional):
//!   SNITCHWATCH_GRPC_BIND  gRPC bind address (default: 127.0.0.1:0)
//!   SNITCHWATCH_WS_BIND    WebSocket bind address (default: 127.0.0.1:0)
//!
//! On startup the CLI prints two machine-parseable lines to stdout so test
//! harnesses and wrapping processes can discover the ports:
//!
//!   GRPC_LISTEN_ADDR=<addr>
//!   WS_LISTEN_ADDR=<addr>
//!
//! All of the orchestration logic lives in `snitchwatch_bridge_cli::run` so
//! integration tests can exercise it without spawning a subprocess.

use anyhow::Result;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = BridgeConfig::from_env()?;
    let bridge = run(config).await?;

    // Machine-parseable lines for test harnesses. Order matters: opensnitchd
    // wrappers grep for GRPC_LISTEN_ADDR first.
    println!("GRPC_LISTEN_ADDR={}", bridge.grpc_addr);
    println!("WS_LISTEN_ADDR={}", bridge.ws_addr);
    println!();
    println!("→ open http://{}/ in your browser", bridge.ws_addr);

    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received");
    bridge.shutdown();
    Ok(())
}
