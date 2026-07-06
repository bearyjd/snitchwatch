//! Bridge CLI — runs the bridge that exposes a gRPC `Ui` server (which
//! opensnitchd dials in to) and a WebSocket server for the GUI front-end.
//!
//! Usage:
//!   snitchwatch-bridge-cli
//!
//! Env vars (all optional):
//!   SNITCHWATCH_GRPC_BIND   gRPC bind address (default: 127.0.0.1:0)
//!   SNITCHWATCH_WS_SOCKET   Unix domain socket path for the WS server
//!                           (default: $XDG_RUNTIME_DIR/snitchwatch/bridge.sock)
//!
//! On startup the CLI prints machine-parseable lines to stdout so test
//! harnesses and wrapping processes (and the GUI shell) can discover the
//! gRPC port, the WS Unix socket, and the handshake token:
//!
//!   GRPC_LISTEN_ADDR=<addr>
//!   WS_SOCKET_PATH=<path>
//!   WS_TOKEN_PATH=<path>
//!
//! The token file is written with mode 0600 under the same directory the
//! socket lives in.
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

    // Machine-parseable lines for test harnesses / the GUI launcher. Order
    // matters: opensnitchd wrappers grep for GRPC_LISTEN_ADDR first.
    println!("GRPC_LISTEN_ADDR={}", bridge.grpc_addr);
    println!("WS_SOCKET_PATH={}", bridge.ws_socket_path.display());
    println!("WS_TOKEN_PATH={}", bridge.ws_token_path.display());

    wait_for_shutdown_signal().await?;
    info!("shutdown signal received");
    bridge.shutdown();
    Ok(())
}

/// Block until the process is asked to stop.
///
/// Handles both SIGINT (Ctrl-C in an interactive shell) and SIGTERM (what
/// `systemctl --user stop snitchwatch-bridge.service` sends). Without the
/// SIGTERM arm, running under systemd would fall back to the default SIGTERM
/// disposition (immediate termination) and skip the clean `bridge.shutdown()`
/// path — the bridge is packaged as a systemd `--user` service, so a graceful
/// SIGTERM stop is the common case, not the exception.
#[cfg(unix)]
async fn wait_for_shutdown_signal() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        res = tokio::signal::ctrl_c() => res?,
        _ = sigterm.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}
