//! In-process bridge runtime.
//!
//! Wraps `snitchwatch_bridge_cli::run` to spawn the bridge on a background
//! tokio runtime and expose the tray + notice receivers to the Tauri shell.
//!
//! The bridge's WS+HTTP server binds a Unix domain socket (see
//! `snitchwatch_bridge::ws_server`), not TCP loopback — so this module also
//! spawns `crate::loopback_proxy` to bridge that Unix socket back to the
//! plain `http://127.0.0.1:PORT/` URL the webview window is configured to
//! load (`tauri.conf.json`'s `app.windows[0].url`), transparently presenting
//! the bridge's handshake token on the `/stream` route's behalf.

use snitchwatch_bridge::notice::Notice;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge_cli::{BridgeConfig, RunningBridge};
use std::net::SocketAddr;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

pub struct BridgeRuntimeConfig {
    /// Loopback TCP address the webview window connects to. The bridge
    /// itself only listens on a Unix domain socket; this is where
    /// `crate::loopback_proxy` binds to bridge the two.
    pub ws_proxy_bind: SocketAddr,
    pub grpc_bind: SocketAddr,
}

impl Default for BridgeRuntimeConfig {
    fn default() -> Self {
        Self {
            ws_proxy_bind: "127.0.0.1:3031".parse().unwrap(),
            grpc_bind: "127.0.0.1:50051".parse().unwrap(),
        }
    }
}

pub struct BridgeRuntime {
    pub bridge: RunningBridge,
    proxy_handle: JoinHandle<()>,
}

impl BridgeRuntime {
    pub fn tray_rx(&self) -> &watch::Receiver<TrayState> {
        &self.bridge.tray_rx
    }

    pub fn notice_rx(&self) -> broadcast::Receiver<Notice> {
        self.bridge.notice_rx.resubscribe()
    }

    pub fn shutdown(self) {
        self.proxy_handle.abort();
        self.bridge.shutdown();
    }
}

pub async fn spawn_bridge_runtime(cfg: BridgeRuntimeConfig) -> anyhow::Result<BridgeRuntime> {
    let bridge_cfg = BridgeConfig {
        grpc_bind: cfg.grpc_bind,
        ws_socket_path: snitchwatch_bridge::auth::runtime_dir().join("bridge.sock"),
        cache_capacity: 10_000,
    };
    let bridge = snitchwatch_bridge_cli::run(bridge_cfg).await?;

    // Bridge the bridge's Unix socket back to the loopback TCP address the
    // webview window is configured to load.
    let socket_path = bridge.ws_socket_path.clone();
    let token = bridge.ws_token.clone();
    let proxy_bind = cfg.ws_proxy_bind;
    let proxy_handle = tokio::spawn(async move {
        if let Err(e) = crate::loopback_proxy::run(proxy_bind, socket_path, token).await {
            tracing::error!(error = %e, "webview loopback proxy exited");
        }
    });

    Ok(BridgeRuntime {
        bridge,
        proxy_handle,
    })
}
