//! Bridge endpoint and runtime configuration.

use anyhow::{Context, Result};
use snitchwatch_bridge::auth;
use std::net::SocketAddr;
use std::path::PathBuf;

/// The actual daemon endpoint. System mode never has a TCP address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrpcEndpoint {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl GrpcEndpoint {
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        match self {
            Self::Tcp(addr) => Some(*addr),
            Self::Unix(_) => None,
        }
    }
}

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
