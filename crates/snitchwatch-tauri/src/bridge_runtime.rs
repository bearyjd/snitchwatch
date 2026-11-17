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
use snitchwatch_bridge_cli::{
    resolve_grpc_listen, BridgeConfig, GrpcListen, RunningBridge, DEV_TCP_ENV,
};
use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;
use std::path::Path;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

pub struct BridgeRuntimeConfig {
    /// Loopback TCP address the webview window connects to. The bridge
    /// itself only listens on a Unix domain socket; this is where
    /// `crate::loopback_proxy` binds to bridge the two.
    pub ws_proxy_bind: SocketAddr,
}

impl Default for BridgeRuntimeConfig {
    fn default() -> Self {
        Self {
            ws_proxy_bind: "127.0.0.1:3031".parse().unwrap(),
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

/// The embed only runs in dev-TCP mode (#35): its webview proxy on loopback
/// hands out the WS token, so without `SNITCHWATCH_DEV_TCP=1` it refuses to
/// start rather than degrade to a half-gated state. Pure over its inputs.
///
/// Also refuses a missing/relative `XDG_RUNTIME_DIR`: `auth::runtime_dir`
/// would otherwise fall back to a shared `/tmp/snitchwatch` for the WS
/// socket and token.
pub fn resolve_embed_listen(
    dev_flag: Option<OsString>,
    grpc_bind: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    system_flag: Option<OsString>,
) -> anyhow::Result<GrpcListen> {
    if dev_flag.as_deref() != Some(OsStr::new("1")) {
        anyhow::bail!(
            "the Tauri desktop shell is dev-only: its webview proxy on loopback TCP exposes the \
             bridge token. Set {DEV_TCP_ENV}=1 to run it; a desktop install uses the system bridge."
        );
    }
    match xdg_runtime_dir.as_deref().map(Path::new) {
        Some(dir) if dir.is_absolute() => {}
        _ => anyhow::bail!(
            "XDG_RUNTIME_DIR must be set to an absolute path: the bridge's WS socket and token \
             must not fall back to a shared temp directory"
        ),
    }
    resolve_grpc_listen(dev_flag, grpc_bind, xdg_runtime_dir, system_flag)
}

pub async fn spawn_bridge_runtime(cfg: BridgeRuntimeConfig) -> anyhow::Result<BridgeRuntime> {
    let grpc_listen = resolve_embed_listen(
        std::env::var_os(DEV_TCP_ENV),
        std::env::var_os("SNITCHWATCH_GRPC_BIND"),
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("SNITCHWATCH_SYSTEM_BRIDGE"),
    )?;
    let bridge_cfg = BridgeConfig {
        grpc_listen,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn refuses_without_the_flag_and_names_it() {
        let err = resolve_embed_listen(None, None, os("/run/user/1000"), None).unwrap_err();
        assert!(err.to_string().contains("SNITCHWATCH_DEV_TCP=1"), "{err}");
    }

    #[test]
    fn refuses_a_non_1_flag_value() {
        assert!(resolve_embed_listen(os("0"), None, os("/run/user/1000"), None).is_err());
    }

    #[test]
    fn with_the_flag_uses_loopback_dev_tcp() {
        let got = resolve_embed_listen(os("1"), None, os("/run/user/1000"), None).unwrap();
        assert_eq!(got, GrpcListen::DevTcp("127.0.0.1:0".parse().unwrap()));
    }

    #[test]
    fn with_the_flag_refuses_a_non_loopback_bind() {
        let r = resolve_embed_listen(os("1"), os("0.0.0.0:50051"), os("/run/user/1000"), None);
        assert!(r.is_err());
    }

    #[test]
    fn with_the_flag_refuses_a_missing_or_relative_runtime_dir() {
        assert!(resolve_embed_listen(os("1"), None, None, None).is_err());
        assert!(resolve_embed_listen(os("1"), None, os("rel"), None).is_err());
    }
}
