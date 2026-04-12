//! WebSocket server for the embedded webview.
//!
//! Binds to a configurable local address (default: a random ephemeral port
//! on 127.0.0.1) and serves the `/stream` endpoint. The Tauri shell reads
//! the actual bound port after startup and points the webview at it.

use crate::blocklists::BlocklistsManager;
use crate::web_assets::{serve_asset, serve_fallback, serve_index};
use crate::ws_messages::{ClientMessage, ServerMessage};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info};

/// Channels the bridge core uses to talk to the WS server.
#[derive(Clone)]
pub struct WsHandles {
    /// Server pushes broadcast to all connected clients.
    pub broadcast: broadcast::Sender<ServerMessage>,
    /// Inbound client messages get forwarded here for the bridge to act on.
    pub inbound: mpsc::Sender<ClientMessage>,
    /// Shared blocklists manager — provides subscription state to WS handlers.
    pub blocklists: Arc<BlocklistsManager>,
}

pub struct WsServer {
    bind: SocketAddr,
    handles: WsHandles,
}

impl WsServer {
    pub fn new(bind: SocketAddr, handles: WsHandles) -> Self {
        Self { bind, handles }
    }

    /// Construct a `WsServer` with an explicit `BlocklistsManager`.
    pub fn new_with_blocklists(
        bind: SocketAddr,
        handles: WsHandles,
        blocklists: Arc<BlocklistsManager>,
    ) -> Self {
        Self {
            bind,
            handles: WsHandles {
                blocklists,
                ..handles
            },
        }
    }

    /// Return a reference to the shared `BlocklistsManager`.
    pub fn blocklists(&self) -> &Arc<BlocklistsManager> {
        &self.handles.blocklists
    }

    /// Bind the listener and return the actual bound address (so callers can
    /// pass `:0` for an ephemeral port and discover what they got).
    pub async fn bind(&self) -> std::io::Result<(TcpListener, SocketAddr)> {
        let listener = TcpListener::bind(self.bind).await?;
        let local = listener.local_addr()?;
        Ok((listener, local))
    }

    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(self.handles);

        info!(addr = ?listener.local_addr()?, "WS+HTTP server starting");
        axum::serve(listener, app).await
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(handles): axum::extract::State<WsHandles>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, handles))
}

async fn handle_socket(socket: WebSocket, handles: WsHandles) {
    use futures_util::{SinkExt, StreamExt};
    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = handles.broadcast.subscribe();

    // Outbound task: forward broadcast messages to this client.
    let outbound = tokio::spawn(async move {
        while let Ok(msg) = broadcast_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    error!(error = %e, "failed to serialize ServerMessage");
                    continue;
                }
            };
            if sender.send(Message::Text(json)).await.is_err() {
                debug!("WS client disconnected (outbound)");
                break;
            }
        }
    });

    // Inbound loop: parse client messages and forward to the bridge.
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                Ok(parsed) => {
                    if handles.inbound.send(parsed).await.is_err() {
                        debug!("inbound channel closed; dropping client");
                        break;
                    }
                }
                Err(e) => {
                    error!(error = %e, raw = %text, "failed to parse ClientMessage");
                }
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    outbound.abort();
    debug!("WS client connection ended");
}

/// Boot a minimal WS server for integration tests. Returns the WebSocket URL
/// and a shutdown handle (abort the handle to stop the server).
pub async fn serve_with_blocklists(
    addr: SocketAddr,
    blocklists: Arc<BlocklistsManager>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let (broadcast_tx, _) = broadcast::channel(16);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(64);
    let handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx,
        blocklists: blocklists.clone(),
    };
    let server = WsServer::new(addr, handles);
    let (listener, bound) = server.bind().await?;

    // Spawn the inbound message handler — routes blocklist actions and broadcasts updates.
    let bl_mgr = blocklists.clone();
    tokio::spawn(async move {
        use crate::translator::upstream::{handle_blocklist_action, BlocklistActionOutcome};
        while let Some(msg) = inbound_rx.recv().await {
            match handle_blocklist_action(bl_mgr.clone(), msg).await {
                Ok(BlocklistActionOutcome::Subscribed { .. }) => {}
                Ok(BlocklistActionOutcome::Unsubscribed { .. }) => {}
                Ok(BlocklistActionOutcome::Unhandled(_)) => {}
                Err(e) => tracing::warn!(error = %e, "blocklist action failed"),
            }
        }
    });

    // Spawn a task that listens to blocklist events and broadcasts ServerMessages.
    let bl_mgr2 = blocklists.clone();
    let bc_tx2 = broadcast_tx.clone();
    let mut bl_rx = blocklists.subscribe();
    tokio::spawn(async move {
        while let Ok(evt) = bl_rx.recv().await {
            match evt {
                crate::blocklists::BlocklistEvent::SubscriptionsChanged => {
                    if let Ok(m) =
                        crate::translator::downstream::build_set_blocklists(&bl_mgr2).await
                    {
                        let _ = bc_tx2.send(m);
                    }
                }
                crate::blocklists::BlocklistEvent::EntriesChanged {
                    ref subscription_id,
                } => {
                    // Broadcast the entries themselves.
                    if let Ok(m) = crate::translator::downstream::build_set_blocklist_entries(
                        &bl_mgr2,
                        subscription_id,
                    )
                    .await
                    {
                        let _ = bc_tx2.send(m);
                    }
                    // Also broadcast an updated summary (entry_count changed).
                    if let Ok(m) =
                        crate::translator::downstream::build_set_blocklists(&bl_mgr2).await
                    {
                        let _ = bc_tx2.send(m);
                    }
                }
                crate::blocklists::BlocklistEvent::StatusChanged {
                    ref subscription_id,
                } => {
                    if let Ok(m) = crate::translator::downstream::build_set_blocklist_status(
                        &bl_mgr2,
                        subscription_id,
                    )
                    .await
                    {
                        let _ = bc_tx2.send(m);
                    }
                }
            }
        }
    });

    // Send initial empty SetBlocklists snapshot to all new connections.
    if let Ok(initial) = crate::translator::downstream::build_set_blocklists(&blocklists).await {
        let _ = broadcast_tx.send(initial);
    }

    let handle = tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    Ok((format!("ws://127.0.0.1:{}/stream", bound.port()), handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_handles() -> WsHandles {
        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let store = Arc::new(crate::blocklists::store::BlocklistStore::open_in_memory().unwrap());
        WsHandles {
            broadcast: broadcast_tx,
            inbound: inbound_tx,
            blocklists: Arc::new(BlocklistsManager::new(store)),
        }
    }

    #[tokio::test]
    async fn server_state_carries_blocklists_manager() {
        use crate::blocklists::store::BlocklistStore;
        use crate::blocklists::BlocklistsManager;
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        let mgr = Arc::new(BlocklistsManager::new(store));
        let handles = default_handles();
        let server =
            WsServer::new_with_blocklists("127.0.0.1:0".parse().unwrap(), handles, mgr.clone());
        assert!(Arc::ptr_eq(server.blocklists(), &mgr));
    }

    #[tokio::test]
    async fn server_binds_to_ephemeral_port() {
        let handles = default_handles();
        let server = WsServer::new("127.0.0.1:0".parse().unwrap(), handles);
        let (_listener, addr) = server.bind().await.unwrap();
        assert_ne!(
            addr.port(),
            0,
            "ephemeral port should resolve to a real port"
        );
    }

    #[tokio::test]
    async fn server_serves_index_html_at_root() {
        use axum::body::to_bytes;
        use axum::http::Request;
        use tower::ServiceExt;

        let handles = default_handles();

        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(handles);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("Snitchwatch"));
    }

    #[tokio::test]
    async fn server_serves_asset_js() {
        use axum::http::Request;
        use tower::ServiceExt;

        let handles = default_handles();
        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(handles);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/assets/js/app.js")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}
