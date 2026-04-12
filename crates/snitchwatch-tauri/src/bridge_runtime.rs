//! In-process bridge runtime.
//!
//! Wraps `snitchwatch_bridge_cli::run` to spawn the bridge on a background
//! tokio runtime and expose the tray + notice receivers to the Tauri shell.

use snitchwatch_bridge::notice::Notice;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge_cli::{BridgeConfig, RunningBridge};
use std::net::SocketAddr;
use tokio::sync::{broadcast, watch};

pub struct BridgeRuntimeConfig {
    pub ws_bind: SocketAddr,
    pub grpc_bind: SocketAddr,
}

impl Default for BridgeRuntimeConfig {
    fn default() -> Self {
        Self {
            ws_bind: "127.0.0.1:3031".parse().unwrap(),
            grpc_bind: "127.0.0.1:50051".parse().unwrap(),
        }
    }
}

pub struct BridgeRuntime {
    pub bridge: RunningBridge,
}

impl BridgeRuntime {
    pub fn tray_rx(&self) -> &watch::Receiver<TrayState> {
        &self.bridge.tray_rx
    }

    pub fn notice_rx(&self) -> broadcast::Receiver<Notice> {
        self.bridge.notice_rx.resubscribe()
    }

    pub fn shutdown(self) {
        self.bridge.shutdown();
    }
}

pub async fn spawn_bridge_runtime(cfg: BridgeRuntimeConfig) -> anyhow::Result<BridgeRuntime> {
    let bridge_cfg = BridgeConfig {
        ws_bind: cfg.ws_bind,
        grpc_bind: cfg.grpc_bind,
        cache_capacity: 10_000,
    };
    let bridge = snitchwatch_bridge_cli::run(bridge_cfg).await?;
    Ok(BridgeRuntime { bridge })
}
