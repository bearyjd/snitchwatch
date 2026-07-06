//! Error types for the bridge.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("rule translation failed: {reason}")]
    RuleTranslation { reason: String },

    #[error("connection cache error: {reason}")]
    Cache { reason: String },

    #[error("blocklist store error: {0}")]
    Blocklist(#[from] crate::blocklists::store::StoreError),

    #[error("daemon disconnected — reconnecting")]
    Disconnected,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
