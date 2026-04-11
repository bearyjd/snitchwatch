//! Bridge orchestrator — testable library entry point.
//!
//! This crate's binary (`main.rs`) is a thin wrapper around [`run`]. Tests
//! construct a [`BridgeConfig`] and call `run` directly so they don't have
//! to spawn the CLI as a subprocess.
//!
//! What `run` wires together:
//!
//! 1. Binds the WebSocket server on `ws_bind` (defaults to `127.0.0.1:3031`).
//! 2. Dials the gRPC endpoint at `grpc_url` with exponential backoff.
//! 3. Opens the `Notifications` bidi stream. Outbound goes through an mpsc
//!    which is fed by the verdict-round-trip task; inbound is the daemon's
//!    notification stream.
//! 4. For each inbound notification, runs [`downstream::translate_notification`]:
//!    - `Translated::AskRule(row)` → insert into the cache as pending, await
//!      the `oneshot<Verdict>`, push an `InsertConnectionRows` broadcast so
//!      the WebSocket UI sees it, then send a `NotificationReply` upstream
//!      so the daemon unblocks.
//!    - `Translated::Ignored` → drop.
//! 5. Inbound WebSocket `ClientMessage`s go through `upstream::apply`, which
//!    mutates the cache (resolving pending rows by firing the oneshot).
//!
//! See the Task 20 note in `downstream.rs` for why ask-rule events ride on
//! the Notifications stream for M1.

use anyhow::{Context, Result};
use snitchwatch_bridge::cache::connections::{ConnectionCache, Verdict};
use snitchwatch_bridge::grpc_client::{GrpcClient, UiClient};
use snitchwatch_bridge::translator::{downstream, downstream::Translated, upstream};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use snitchwatch_proto::protocol::{NotificationReply, NotificationReplyCode};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

/// Runtime configuration for [`run`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// gRPC endpoint to dial. e.g. `http://127.0.0.1:50051`.
    pub grpc_url: String,
    /// Address to bind the WebSocket server on. Defaults to `127.0.0.1:3031`; use port `0` for ephemeral (tests).
    pub ws_bind: SocketAddr,
    /// Cache capacity (number of recent rows retained).
    pub cache_capacity: usize,
}

impl BridgeConfig {
    pub fn from_env() -> Result<Self> {
        let grpc_url = std::env::var("SNITCHWATCH_GRPC")
            .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string());
        let ws_bind_str =
            std::env::var("SNITCHWATCH_WS_BIND").unwrap_or_else(|_| "127.0.0.1:3031".to_string());
        let ws_bind: SocketAddr = ws_bind_str
            .parse()
            .with_context(|| format!("invalid SNITCHWATCH_WS_BIND: {ws_bind_str}"))?;
        Ok(Self {
            grpc_url,
            ws_bind,
            cache_capacity: 10_000,
        })
    }
}

/// Handle to a running bridge. Dropping this does **not** shut the bridge
/// down — call [`RunningBridge::shutdown`] explicitly when you're done.
pub struct RunningBridge {
    /// Actual bound WebSocket address (so callers who passed `:0` can discover it).
    pub ws_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl RunningBridge {
    /// Signal every background task to stop. Safe to call more than once.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Start the bridge and return as soon as every background task is running.
///
/// The WebSocket server is up and accepting connections by the time this
/// returns, but the gRPC `Notifications` stream may still be in the middle
/// of its initial handshake. That's fine — scripted mock events are buffered
/// on the server side and replayed once the stream opens.
pub async fn run(config: BridgeConfig) -> Result<RunningBridge> {
    info!(?config, "starting snitchwatch-bridge");

    // Channels between the WS server and the orchestrator.
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(256);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(256);

    // Shared connection cache (pending-row state + decided-row history).
    let cache = Arc::new(Mutex::new(ConnectionCache::new(config.cache_capacity)));

    // --- WebSocket server ---------------------------------------------------
    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx,
    };
    let ws_server = WsServer::new(config.ws_bind, ws_handles);
    let (listener, ws_addr) = ws_server
        .bind()
        .await
        .context("failed to bind WebSocket listener")?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            res = ws_server.serve(listener) => {
                if let Err(e) = res {
                    error!(error = %e, "ws_server::serve exited");
                }
            }
            _ = shutdown_rx => {
                info!("ws server shutdown signal received");
            }
        }
    });

    // --- gRPC channel + notifications stream --------------------------------
    let client = GrpcClient::new(config.grpc_url.clone());
    let channel = client
        .connect_with_backoff()
        .await
        .context("failed to connect to opensnitchd gRPC")?;

    // Outbound NotificationReply channel: the downstream pump sends replies
    // into this mpsc, which is wrapped as a Stream and handed to the
    // Notifications bidi RPC.
    let (reply_tx, reply_rx) = mpsc::channel::<NotificationReply>(64);
    let outbound_stream = ReceiverStream::new(reply_rx);

    let mut ui_client = UiClient::new(channel);
    let inbound_stream = ui_client
        .notifications(outbound_stream)
        .await
        .context("failed to open Notifications bidi stream")?
        .into_inner();

    // --- Downstream pump: notifications → cache → WS broadcast --------------
    let cache_for_downstream = cache.clone();
    let broadcast_for_downstream = broadcast_tx.clone();
    let reply_tx_for_downstream = reply_tx.clone();
    tokio::spawn(async move {
        downstream_pump(
            inbound_stream,
            cache_for_downstream,
            broadcast_for_downstream,
            reply_tx_for_downstream,
        )
        .await;
    });

    // --- Upstream pump: WS client messages → cache (→ oneshot resolve) -------
    let cache_for_upstream = cache.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let mut cache = cache_for_upstream.lock().await;
            match upstream::apply(&mut cache, msg) {
                Ok(effect) => {
                    info!(?effect, "applied upstream effect");
                    // TODO (M2): perform gRPC side effects here (AddRule,
                    // DeleteRule, UpdateRule). The verdict side effect is
                    // already covered by cache.resolve firing the oneshot.
                }
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    Ok(RunningBridge {
        ws_addr,
        shutdown_tx: Some(shutdown_tx),
    })
}

/// Downstream pump: read each Notification, translate, insert-pending into
/// the cache, broadcast to WS, await the verdict oneshot, then send a
/// NotificationReply upstream.
async fn downstream_pump(
    mut inbound: tonic::Streaming<snitchwatch_proto::protocol::Notification>,
    cache: Arc<Mutex<ConnectionCache>>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
    reply_tx: mpsc::Sender<NotificationReply>,
) {
    loop {
        let msg = match inbound.message().await {
            Ok(Some(n)) => n,
            Ok(None) => {
                info!("notifications stream closed by daemon");
                return;
            }
            Err(e) => {
                warn!(error = %e, "notifications stream error");
                return;
            }
        };

        let notification_id = msg.id;
        match downstream::translate_notification(&msg) {
            Translated::Ignored => {
                // Nothing to do.
            }
            Translated::AskRule(boxed_row) => {
                let row = *boxed_row;
                // 1. Insert as pending and grab the verdict receiver.
                let verdict_rx = {
                    let mut cache = cache.lock().await;
                    cache.insert_pending(row.clone())
                };

                // 2. Broadcast to any connected WS clients so the UI shows it.
                if broadcast_tx.receiver_count() > 0 {
                    let broadcast_msg = ServerMessage::InsertConnectionRows {
                        rows: vec![row.clone()],
                    };
                    if let Err(e) = broadcast_tx.send(broadcast_msg) {
                        warn!(error = %e, "broadcast send failed (no subscribers?)");
                    }
                }

                // 3. Spawn a task to await the verdict and send the reply.
                //    We spawn so a slow user doesn't stall the downstream pump.
                let reply_tx = reply_tx.clone();
                tokio::spawn(async move {
                    match verdict_rx.await {
                        Ok(verdict) => {
                            let reply = NotificationReply {
                                id: notification_id,
                                code: NotificationReplyCode::Ok as i32,
                                data: verdict_to_json(verdict),
                            };
                            if let Err(e) = reply_tx.send(reply).await {
                                warn!(error = %e, "failed to send NotificationReply");
                            }
                        }
                        Err(_canceled) => {
                            warn!(
                                notification_id,
                                "verdict oneshot dropped before it was resolved"
                            );
                        }
                    }
                });
            }
        }
    }
}

fn verdict_to_json(v: Verdict) -> String {
    match v {
        Verdict::Allow => r#"{"verdict":"allow"}"#.to_string(),
        Verdict::Deny => r#"{"verdict":"deny"}"#.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_allow_serializes() {
        assert_eq!(verdict_to_json(Verdict::Allow), r#"{"verdict":"allow"}"#);
    }

    #[test]
    fn verdict_deny_serializes() {
        assert_eq!(verdict_to_json(Verdict::Deny), r#"{"verdict":"deny"}"#);
    }
}
