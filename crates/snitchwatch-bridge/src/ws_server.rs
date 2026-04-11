//! WebSocket server for the embedded webview.
//!
//! Binds to a configurable local address (default: a random ephemeral port
//! on 127.0.0.1) and serves the `/stream` endpoint. The Tauri shell reads
//! the actual bound port after startup and points the webview at it.

use crate::web_assets::{serve_asset, serve_fallback, serve_index};
use crate::ws_messages::{ClientMessage, ServerMessage};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::net::SocketAddr;
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
}

pub struct WsServer {
    bind: SocketAddr,
    handles: WsHandles,
}

impl WsServer {
    pub fn new(bind: SocketAddr, handles: WsHandles) -> Self {
        Self { bind, handles }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_binds_to_ephemeral_port() {
        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let handles = WsHandles {
            broadcast: broadcast_tx,
            inbound: inbound_tx,
        };
        let server = WsServer::new("127.0.0.1:0".parse().unwrap(), handles);
        let (_listener, addr) = server.bind().await.unwrap();
        assert_ne!(addr.port(), 0, "ephemeral port should resolve to a real port");
    }

    #[tokio::test]
    async fn server_serves_index_html_at_root() {
        use axum::body::to_bytes;
        use axum::http::Request;
        use tower::ServiceExt;

        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let handles = WsHandles {
            broadcast: broadcast_tx,
            inbound: inbound_tx,
        };

        let app = Router::new()
            .route("/stream", get(ws_handler))
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback)
            .with_state(handles);

        let response = app
            .oneshot(Request::builder().uri("/").body(axum::body::Body::empty()).unwrap())
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

        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let handles = WsHandles {
            broadcast: broadcast_tx,
            inbound: inbound_tx,
        };
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
