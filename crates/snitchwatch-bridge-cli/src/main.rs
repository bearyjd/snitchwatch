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

    // Machine-parseable line for test harnesses.
    println!("WS_LISTEN_ADDR={}", bridge.ws_addr);

    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received");
    bridge.shutdown();
    Ok(())
}
