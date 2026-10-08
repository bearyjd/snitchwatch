//! WebSocket server for the embedded webview.
//!
//! Binds to a Unix domain socket under `$XDG_RUNTIME_DIR/snitchwatch/`
//! (mode 0700 dir, 0600 socket file) and serves the `/stream` endpoint. The
//! GUI shell reads the token file written alongside the socket (see
//! `crate::auth`) and presents it as the first WS text frame after upgrade,
//! before sending any `ClientMessage`.
//!
//! A Unix domain socket (rather than TCP loopback) is required so a
//! Flatpak-sandboxed GUI can reach the bridge at all: Flatpak's default
//! sandbox gets its own private network namespace, so `127.0.0.1:PORT`
//! never crosses the sandbox boundary regardless of auth, whereas a Unix
//! socket under `$XDG_RUNTIME_DIR` does via the well-precedented
//! `--filesystem=xdg-run/<name>` permission.
//!
//! Only `/stream` requires the handshake token — `/`, `/assets/*`, and the
//! SPA fallback only serve static frontend assets (no firewall-rule-writing
//! messages flow through them), so they stay unauthenticated. Those static
//! routes exist only with the `web-ui` feature (on by default); without it —
//! the release tarball's build — every path but `/stream` is a plain 404.

use crate::auth::Token;
use crate::blocklists::BlocklistsManager;
use crate::profiles::ProfilesManager;
#[cfg(feature = "web-ui")]
use crate::web_assets::{serve_asset, serve_fallback, serve_index};
use crate::ws_messages::{ClientMessage, ServerMessage};
use anyhow::Context;
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::get,
    Extension, Router,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

/// Channels the bridge core uses to talk to the WS server.
#[derive(Clone)]
pub struct WsHandles {
    /// Server pushes broadcast to all connected clients.
    pub broadcast: broadcast::Sender<ServerMessage>,
    /// Authenticated external sessions only, never broadcast subscribers.
    pub presence: crate::client_presence::ClientPresence,
    /// Inbound client messages get forwarded here for the bridge to act on.
    pub inbound: mpsc::Sender<ClientMessage>,
    /// Shared blocklists manager — provides subscription state to WS handlers.
    pub blocklists: Arc<BlocklistsManager>,
    /// Shared profiles manager — provides profile state to WS handlers.
    pub profiles: Arc<ProfilesManager>,
}

/// State handed to axum's router: the WS channels plus the handshake token
/// every `/stream` connection must present before it's trusted.
#[derive(Clone)]
struct AppState {
    handles: WsHandles,
    token: Token,
}

/// The connecting peer's uid from `SO_PEERCRED`, captured at accept so the
/// bridge can log which session paused filtering (issue #47). `None` if the
/// kernel could not report it.
#[derive(Clone, Copy, Debug)]
struct PeerUid(Option<u32>);

pub struct WsServer {
    socket_path: PathBuf,
    token: Token,
    handles: WsHandles,
}

impl WsServer {
    pub fn new(socket_path: PathBuf, token: Token, handles: WsHandles) -> Self {
        Self {
            socket_path,
            token,
            handles,
        }
    }

    /// Construct a `WsServer` with an explicit `BlocklistsManager`.
    pub fn new_with_blocklists(
        socket_path: PathBuf,
        token: Token,
        handles: WsHandles,
        blocklists: Arc<BlocklistsManager>,
    ) -> Self {
        Self {
            socket_path,
            token,
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

    /// Bind the Unix domain socket listener.
    ///
    /// Creates the parent directory (mode 0700) if it doesn't exist yet,
    /// removes a stale socket file left behind by a previous run (otherwise
    /// `bind` fails with `AddrInUse`), and tightens the socket file itself
    /// to mode 0600 once bound.
    pub async fn bind(&self) -> std::io::Result<UnixListener> {
        if let Some(parent) = self.socket_path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        if self.socket_path.exists() {
            fs::remove_file(&self.socket_path)?;
        }
        let listener = UnixListener::bind(&self.socket_path)?;
        fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    /// The HTTP surface: the token-gated `/stream` WebSocket, plus — only
    /// with the `web-ui` feature — the embedded static frontend (`/`,
    /// `/assets/*`, SPA fallback). Without the feature, every other path is a
    /// plain 404.
    fn router(state: AppState) -> Router {
        let app = Router::new().route("/stream", get(ws_handler));
        #[cfg(feature = "web-ui")]
        let app = app
            .route("/", get(serve_index))
            .route("/assets/*path", get(serve_asset))
            .fallback(serve_fallback);
        app.with_state(state)
    }

    /// Serve the router over `listener`.
    ///
    /// `axum::serve` in axum 0.7 only accepts a `tokio::net::TcpListener`,
    /// so serving over a `UnixListener` means driving hyper directly —
    /// this mirrors axum's own `unix-domain-socket` example: accept a
    /// connection, wrap it for hyper via `TokioIo`, and hand it to
    /// `hyper_util`'s auto (h1/h2) connection builder with upgrade support
    /// (required for the WebSocket `Upgrade` handshake on `/stream`).
    pub async fn serve(self, listener: UnixListener) -> std::io::Result<()> {
        info!(socket = ?self.socket_path, "WS+HTTP server starting");
        let state = AppState {
            handles: self.handles,
            token: self.token,
        };
        let app = Self::router(state);

        loop {
            let (stream, _peer_addr) = listener.accept().await?;
            let peer_uid = PeerUid(stream.peer_cred().ok().map(|cred| cred.uid()));
            let tower_service = app.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let hyper_service = hyper::service::service_fn(
                    move |mut request: hyper::Request<hyper::body::Incoming>| {
                        request.extensions_mut().insert(peer_uid);
                        tower::Service::call(&mut tower_service.clone(), request)
                    },
                );
                if let Err(err) = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .serve_connection_with_upgrades(io, hyper_service)
                .await
                {
                    debug!(error = %err, "connection error");
                }
            });
        }
    }
}

/// Largest WebSocket message (and frame) a client may send. Client messages
/// are small (a blocklist URL is at most 2 KiB); axum's default is 64 MiB.
pub const MAX_CLIENT_MESSAGE_BYTES: usize = 1024 * 1024;

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    peer: Option<Extension<PeerUid>>,
) -> impl IntoResponse {
    let peer_uid = peer.and_then(|Extension(PeerUid(uid))| uid);
    ws.max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .max_frame_size(MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state.handles, state.token, peer_uid))
}

/// Wait for the handshake token as the first WS frame. Returns `true` if the
/// client presented the correct token and the connection should proceed,
/// `false` if it should be rejected (in which case the caller closes it and
/// does no further processing).
async fn await_handshake<S>(receiver: &mut S, token: &Token) -> bool
where
    S: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    use futures_util::StreamExt;
    match receiver.next().await {
        Some(Ok(Message::Text(presented))) if token.matches(&presented) => true,
        Some(Ok(_)) => {
            warn!("WS client rejected: invalid or missing handshake token");
            false
        }
        Some(Err(e)) => {
            warn!(error = %e, "WS handshake read failed");
            false
        }
        None => {
            debug!("WS client disconnected before completing handshake");
            false
        }
    }
}

async fn handle_socket(socket: WebSocket, handles: WsHandles, token: Token, peer_uid: Option<u32>) {
    use futures_util::{SinkExt, StreamExt};
    let (mut sender, mut receiver) = socket.split();

    if !await_handshake(&mut receiver, &token).await {
        let _ = sender.send(Message::Close(None)).await;
        return;
    }

    // A successful write of the token does not prove to a client that the
    // server accepted it: the peer could close immediately after reading it.
    // Send an explicit, per-connection acknowledgement before subscribing to
    // broadcast traffic, making it the first server frame after a successful
    // handshake.
    let authenticated = match serde_json::to_string(&ServerMessage::Authenticated {
        capabilities: crate::bridge_capabilities::advertised(),
    }) {
        Ok(message) => message,
        Err(error) => {
            error!(error = %error, "failed to serialize authentication acknowledgement");
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
    };
    if sender.send(Message::Text(authenticated)).await.is_err() {
        debug!("WS client disconnected before authentication acknowledgement");
        return;
    }

    pump_authenticated(sender, receiver, handles, peer_uid).await;
}

/// Kept as sibling futures: failure of either direction releases the lease
/// and drops the other future, even when its peer never sends another frame.
async fn pump_authenticated<S, R>(
    sender: S,
    mut receiver: R,
    handles: WsHandles,
    peer_uid: Option<u32>,
) where
    S: futures_util::Sink<Message> + Unpin,
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    use futures_util::StreamExt;
    let broadcast_rx = handles.broadcast.subscribe();
    // Answers meant for this connection only (rule import/export, P2.7).
    let (reply_tx, reply_rx) = mpsc::channel::<ServerMessage>(REPLY_QUEUE);
    let _session = handles.presence.authenticated_session();
    // Stable while `_session` is held: the generation only advances when the
    // last authenticated session ends.
    let generation = handles.presence.current_generation();
    let outbound = forward_outbound(sender, broadcast_rx, reply_rx);
    let inbound = async {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                // `ws_handler` sets the same bound on the transport; this
                // holds for any stream (rule import, P2.7).
                Message::Text(text) if text.len() > MAX_CLIENT_MESSAGE_BYTES => {
                    warn!(
                        bytes = text.len(),
                        "dropping an oversized client message unparsed"
                    )
                }
                Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(parsed) => {
                        let parsed = stamp_sender(parsed, generation, peer_uid);
                        let parsed = stamp_reply(parsed, &reply_tx);
                        if handles.inbound.send(parsed).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => error!(%error, "failed to parse ClientMessage"),
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    tokio::select! {
        _ = outbound => {},
        _ = inbound => {},
    }
    debug!("WS client connection ended");
}

/// Send this connection everything broadcast, and the answers meant for it
/// alone, until either ends or the client goes away.
async fn forward_outbound<S>(
    mut sender: S,
    mut broadcast_rx: broadcast::Receiver<ServerMessage>,
    mut reply_rx: mpsc::Receiver<ServerMessage>,
) where
    S: futures_util::Sink<Message> + Unpin,
{
    use futures_util::SinkExt;
    loop {
        let msg = tokio::select! {
            received = broadcast_rx.recv() => match received {
                Ok(msg) => msg,
                Err(_) => break,
            },
            Some(msg) = reply_rx.recv() => msg,
        };
        let json = match serde_json::to_string(&msg) {
            Ok(json) => json,
            Err(error) => {
                error!(%error, "failed to serialize ServerMessage");
                continue;
            }
        };
        if sender.send(Message::Text(json)).await.is_err() {
            break;
        }
    }
}

/// Answers queued for one connection before its sender waits.
const REPLY_QUEUE: usize = 32;

/// Give a rule import/export request a channel back to its own connection.
/// Overwrites whatever it carried; the field is never deserialized anyway.
fn stamp_reply(message: ClientMessage, reply_tx: &mpsc::Sender<ServerMessage>) -> ClientMessage {
    let reply = Some(crate::ws_messages::ReplyTo(reply_tx.clone()));
    match message {
        ClientMessage::ExportRules { request_id, .. } => {
            ClientMessage::ExportRules { request_id, reply }
        }
        ClientMessage::PreviewRulesImport {
            request_id,
            document,
            ..
        } => ClientMessage::PreviewRulesImport {
            request_id,
            document,
            reply,
        },
        ClientMessage::ApplyRulesImport {
            request_id,
            preview_id,
            include,
            ..
        } => ClientMessage::ApplyRulesImport {
            request_id,
            preview_id,
            include,
            reply,
        },
        other => other,
    }
}

/// Stamp a pause request with its sender's GUI-session generation, so
/// `client_presence::apply_pause_request` ignores it once every GUI of that
/// generation has left (issue #47), and with its uid, so the outcome log
/// names who asked. Overwrites whatever the message carried; neither field is
/// ever deserialized from the wire anyway.
fn stamp_sender(message: ClientMessage, generation: u64, peer_uid: Option<u32>) -> ClientMessage {
    match message {
        ClientMessage::SetFilteringPaused {
            paused,
            duration_secs,
            ..
        } => {
            debug!(
                uid = ?peer_uid,
                session_generation = generation,
                paused,
                ?duration_secs,
                "GUI session requested a filtering pause change"
            );
            ClientMessage::SetFilteringPaused {
                paused,
                duration_secs,
                sender_generation: Some(generation),
                sender_uid: peer_uid,
            }
        }
        other => other,
    }
}

/// Boot a minimal WS server for integration tests. Returns the Unix socket
/// path, the handshake token, and a shutdown handle (abort the handle to
/// stop the server).
pub async fn serve_with_blocklists(
    socket_path: PathBuf,
    blocklists: Arc<BlocklistsManager>,
) -> anyhow::Result<(PathBuf, Token, tokio::task::JoinHandle<()>)> {
    let (broadcast_tx, _) = broadcast::channel(16);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(64);
    // This helper is blocklists-focused (see `blocklists_e2e.rs`); profiles
    // get a private in-memory manager so `WsHandles` stays fully populated
    // without changing this function's public signature.
    let profiles = Arc::new(ProfilesManager::new(Arc::new(
        crate::profiles::store::ProfileStore::open_in_memory()
            .context("failed to open in-memory profile store")?,
    )));
    let handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        presence: Default::default(),
        inbound: inbound_tx,
        blocklists: blocklists.clone(),
        profiles,
    };
    let token = Token::generate();
    let server = WsServer::new(socket_path.clone(), token.clone(), handles);
    let listener = server.bind().await?;

    // The same blocklist worker and event pump the production bridge runs.
    let events = crate::blocklists::spawn_event_pump(blocklists.clone(), broadcast_tx.clone());
    let (worker, worker_handle) = crate::blocklists::worker::BlocklistWorker::spawn(blocklists);
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let _ = worker.try_route(msg);
        }
        events.abort();
        worker_handle.abort();
    });

    let handle = tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    Ok((socket_path, token, handle))
}

#[cfg(test)]
#[path = "ws_server/tests.rs"]
mod tests;
