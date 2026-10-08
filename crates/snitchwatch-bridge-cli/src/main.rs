//! Bridge CLI — runs the bridge that exposes a gRPC `Ui` server (which
//! opensnitchd dials in to) and a WebSocket server for the GUI front-end.
//!
//! Usage:
//!   snitchwatch-bridge-cli [-h | --help] [-V | --version]
//!
//! `--help` and `--version` print and exit 0 *before* the async runtime,
//! tracing, or any socket/token/bind exist: `main` is synchronous and classifies
//! argv first, and only builds the tokio runtime for a real bridge start. A
//! closed stdout (`--help | head -c0`) is not an error. Every other argument is
//! ignored.
//!
//! Env vars (all optional):
//!   SNITCHWATCH_GRPC_BIND   gRPC bind address (default: 127.0.0.1:0)
//!   SNITCHWATCH_WS_SOCKET   Unix domain socket path for the WS server
//!                           (default: $XDG_RUNTIME_DIR/snitchwatch/bridge.sock)
//!   SNITCHWATCH_SYSTEM_BRIDGE=1  Strict systemd socket activation: root-only
//!                           daemon Unix socket and group-accessible GUI socket.
//!   STATE_DIRECTORY         Set by systemd's `StateDirectory=`: where blocklist
//!                           subscriptions persist (`blocklists.sqlite3`). The
//!                           system bridge accepts only /var/lib/snitchwatch.
//!   SNITCHWATCH_STATE_DIR   Used when STATE_DIRECTORY is unset. With neither,
//!                           subscriptions are kept in memory only.
//!
//! On startup the CLI prints machine-parseable lines to stdout so test
//! harnesses and wrapping processes (and the GUI shell) can discover the
//! gRPC port, the WS Unix socket, and the handshake token:
//!
//!   GRPC_LISTEN_ADDR=<addr>
//!   WS_SOCKET_PATH=<path>
//!   WS_TOKEN_PATH=<path>
//! System mode reports GRPC_SOCKET_PATH instead of GRPC_LISTEN_ADDR and writes
//! its token to /run/snitchwatch-auth/token with mode 0640.
//!
//! The token file is written with mode 0600 under the same directory the
//! socket lives in.
//!
//! All of the orchestration logic lives in `snitchwatch_bridge_cli::run` so
//! integration tests can exercise it without spawning a subprocess.

use std::io::{ErrorKind, Write};

use anyhow::{Context, Result};
use snitchwatch_bridge_cli::cli::{self, EarlyExit};
use snitchwatch_bridge_cli::{
    activation, resolve_storage, run_system, run_with_options, BridgeConfig, BridgeMode,
    GrpcEndpoint, RunOptions,
};
use tracing::info;

fn main() -> Result<()> {
    // Before anything else — before the async runtime, tracing, or any
    // socket/token/bind: these flags must never touch the socket, the token
    // file, or the gRPC port (a bridge may already be running with them).
    if let Some(early) = cli::early_exit(std::env::args_os()) {
        return match print_early_exit(early) {
            // The reader went away (`--help | head -c0`): nothing left to say.
            Err(e) if e.kind() != ErrorKind::BrokenPipe => {
                Err(anyhow::Error::new(e).context("failed to write to stdout"))
            }
            _ => Ok(()),
        };
    }

    // The same runtime `#[tokio::main]` would build.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build the tokio runtime")?
        .block_on(run_bridge())
}

/// Write the `--help` / `--version` text to stdout. Not `print!`: that panics
/// on a closed pipe (Rust ignores SIGPIPE, so the write fails with EPIPE).
fn print_early_exit(early: EarlyExit) -> std::io::Result<()> {
    let text = match early {
        EarlyExit::Help => cli::usage(),
        EarlyExit::Version => format!("{}\n", cli::version_line()),
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

async fn run_bridge() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bridge = match std::env::var_os("SNITCHWATCH_SYSTEM_BRIDGE") {
        Some(mode) if mode == std::ffi::OsStr::new("1") => {
            run_system(activation::load().context("system socket activation failed")?).await?
        }
        Some(_) => anyhow::bail!("SNITCHWATCH_SYSTEM_BRIDGE must be 1 when set"),
        None => {
            let options = RunOptions {
                storage: resolve_storage(BridgeMode::User),
                blocklist_fetcher: None,
            };
            run_with_options(BridgeConfig::from_env()?, options).await?
        }
    };

    // Machine-parseable lines for test harnesses / the GUI launcher. Order
    // matters: opensnitchd wrappers grep for GRPC_LISTEN_ADDR first.
    match &bridge.grpc_endpoint {
        GrpcEndpoint::Tcp(addr) => println!("GRPC_LISTEN_ADDR={addr}"),
        GrpcEndpoint::Unix(path) => println!("GRPC_SOCKET_PATH={}", path.display()),
    }
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
