//! gRPC client to opensnitchd, with exponential-backoff reconnect.
//!
//! This module only provides the low-level connection helper. The actual
//! notification stream subscription (translating opensnitchd `Notification`
//! events into `ServerMessage` and feeding them to the WS server) is wired
//! up in Task 16 (downstream translator) once we have a clearer picture
//! from the M0 spike about the exact stream RPC shapes.

use crate::error::BridgeError;
use std::time::Duration;
use tonic::transport::{Channel, Endpoint};
use tracing::{info, warn};

// The generated client lives at `snitchwatch_proto::protocol::ui_client::UiClient`
// (the proto declares `package protocol; service UI`). Task 16 will wire the
// actual RPC calls; for now we only need `Channel` here.
pub use snitchwatch_proto::protocol::ui_client::UiClient;

pub struct GrpcClient {
    endpoint: String,
}

impl GrpcClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Connect with exponential backoff. Returns the connected channel.
    ///
    /// The caller can then construct a `UiClient::new(channel)` to issue RPCs.
    pub async fn connect_with_backoff(&self) -> Result<Channel, BridgeError> {
        let mut delay = Duration::from_millis(500);
        let max_delay = Duration::from_secs(60);

        loop {
            match Endpoint::from_shared(self.endpoint.clone())?
                .connect_timeout(Duration::from_secs(3))
                .connect()
                .await
            {
                Ok(channel) => {
                    info!(endpoint = %self.endpoint, "connected to opensnitchd");
                    return Ok(channel);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        retry_in_ms = delay.as_millis() as u64,
                        "gRPC connect failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(max_delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_endpoint_eventually_errors() {
        // We can't easily test the infinite loop, but we can test that a
        // malformed endpoint surfaces an error before we enter the retry
        // loop — `Endpoint::from_shared` parses the URI up front.
        // Spaces are not valid URI characters, so this must reject.
        let client = GrpcClient::new("not a url");
        let result = Endpoint::from_shared(client.endpoint().to_string());
        assert!(result.is_err(), "malformed URI should be rejected");
    }

    #[tokio::test]
    async fn valid_endpoint_parses() {
        // A well-formed http endpoint should parse without error, even if
        // we never actually call `.connect()` on it.
        let client = GrpcClient::new("http://127.0.0.1:50051");
        let result = Endpoint::from_shared(client.endpoint().to_string());
        assert!(result.is_ok(), "valid URI should parse");
    }
}
