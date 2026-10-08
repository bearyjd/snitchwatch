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
    Router,
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
            let tower_service = app.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let hyper_service = hyper::service::service_fn(move |request| {
                    tower::Service::call(&mut tower_service.clone(), request)
                });
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

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.handles, state.token))
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

async fn handle_socket(socket: WebSocket, handles: WsHandles, token: Token) {
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
    let authenticated = match serde_json::to_string(&ServerMessage::Authenticated) {
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

    pump_authenticated(sender, receiver, handles).await;
}

/// Kept as sibling futures: failure of either direction releases the lease
/// and drops the other future, even when its peer never sends another frame.
async fn pump_authenticated<S, R>(mut sender: S, mut receiver: R, handles: WsHandles)
where
    S: futures_util::Sink<Message> + Unpin,
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    let mut broadcast_rx = handles.broadcast.subscribe();
    let _session = handles.presence.authenticated_session();
    let outbound = async move {
        while let Ok(msg) = broadcast_rx.recv().await {
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
    };
    let inbound = async {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(parsed) => {
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
    Ok((socket_path, token, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::UnixStream;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    fn socket_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("bridge.sock")
    }

    fn default_handles() -> WsHandles {
        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let store = Arc::new(crate::blocklists::store::BlocklistStore::open_in_memory().unwrap());
        let profile_store =
            Arc::new(crate::profiles::store::ProfileStore::open_in_memory().unwrap());
        WsHandles {
            broadcast: broadcast_tx,
            presence: Default::default(),
            inbound: inbound_tx,
            blocklists: Arc::new(BlocklistsManager::new(store)),
            profiles: Arc::new(crate::profiles::ProfilesManager::new(profile_store)),
        }
    }

    /// Spawn a `WsServer` bound to a fresh Unix socket in `dir`, returning
    /// the socket path plus the broadcast sender / inbound receiver so tests
    /// can drive and observe it.
    async fn spawn_server_with_inbound(
        dir: &tempfile::TempDir,
        token: Token,
    ) -> (
        PathBuf,
        broadcast::Sender<ServerMessage>,
        mpsc::Receiver<ClientMessage>,
        tokio::task::JoinHandle<()>,
    ) {
        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, inbound_rx) = mpsc::channel(16);
        let store = Arc::new(crate::blocklists::store::BlocklistStore::open_in_memory().unwrap());
        let profile_store =
            Arc::new(crate::profiles::store::ProfileStore::open_in_memory().unwrap());
        let handles = WsHandles {
            broadcast: broadcast_tx.clone(),
            presence: Default::default(),
            inbound: inbound_tx,
            blocklists: Arc::new(BlocklistsManager::new(store)),
            profiles: Arc::new(crate::profiles::ProfilesManager::new(profile_store)),
        };

        let path = socket_path(dir);
        let server = WsServer::new(path.clone(), token, handles);
        let listener = server.bind().await.expect("bind should succeed");
        let join = tokio::spawn(async move {
            let _ = server.serve(listener).await;
        });
        (path, broadcast_tx, inbound_rx, join)
    }

    async fn connect(path: &PathBuf) -> tokio_tungstenite::WebSocketStream<UnixStream> {
        // Retry briefly: the listener may not have finished binding yet
        // since `spawn_server` returns as soon as the spawn is scheduled.
        let mut last_err = None;
        for _ in 0..50 {
            match UnixStream::connect(path).await {
                Ok(stream) => {
                    let (ws, _resp) =
                        tokio_tungstenite::client_async("ws://localhost/stream", stream)
                            .await
                            .expect("ws handshake should succeed");
                    return ws;
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
        panic!("failed to connect to unix socket: {last_err:?}");
    }

    #[tokio::test]
    async fn only_acknowledged_authenticated_clients_establish_presence() {
        let dir = tempfile::tempdir().unwrap();
        let handles = default_handles();
        let presence = handles.presence.clone();
        let token = Token::generate();
        let path = socket_path(&dir);
        let server = WsServer::new(path.clone(), token.clone(), handles);
        let listener = server.bind().await.unwrap();
        let server = tokio::spawn(server.serve(listener));
        let mut stalled = connect(&path).await;
        assert!(presence.admit().is_none());
        let mut invalid = connect(&path).await;
        invalid
            .send(TMessage::Text("invalid-token".into()))
            .await
            .unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), invalid.next())
            .await
            .unwrap();
        assert!(presence.admit().is_none());
        let mut first = connect(&path).await;
        first
            .send(TMessage::Text(token.as_str().into()))
            .await
            .unwrap();
        let ack = first.next().await.unwrap().unwrap();
        assert!(matches!(ack, TMessage::Text(ref text) if text.contains("authenticated")));
        let mut second = connect(&path).await;
        second
            .send(TMessage::Text(token.as_str().into()))
            .await
            .unwrap();
        let _ = second.next().await.unwrap().unwrap();
        let mut admitted = presence.admit().unwrap();
        first.close(None).await.unwrap();
        // Second authenticated client keeps the request admitted; stalled
        // and invalid transports must not affect this count.
        tokio::task::yield_now().await;
        assert!(admitted.while_current(|| ()).is_some());
        second.close(None).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), admitted.lost())
            .await
            .unwrap();
        assert!(presence.admit().is_none());
        stalled.close(None).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn outbound_failure_releases_presence_with_stalled_inbound() {
        let handles = default_handles();
        let presence = handles.presence.clone();
        let broadcast = handles.broadcast.clone();
        let failing_sink = Box::pin(futures_util::sink::unfold(
            (),
            |(), _message: Message| async {
                Err::<(), std::io::Error>(std::io::Error::other("outbound failed"))
            },
        ));
        let never_receives = futures_util::stream::pending::<Result<Message, axum::Error>>();
        let pump = tokio::spawn(pump_authenticated(failing_sink, never_receives, handles));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while presence.admit().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut admission = presence.admit().unwrap();
        broadcast.send(ServerMessage::Authenticated).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), pump)
            .await
            .unwrap()
            .unwrap();
        admission.lost().await;
        assert!(presence.admit().is_none());
    }

    #[tokio::test]
    async fn server_state_carries_blocklists_manager() {
        use crate::blocklists::store::BlocklistStore;
        use crate::blocklists::BlocklistsManager;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        let mgr = Arc::new(BlocklistsManager::new(store));
        let handles = default_handles();
        let server = WsServer::new_with_blocklists(
            socket_path(&dir),
            Token::generate(),
            handles,
            mgr.clone(),
        );
        assert!(Arc::ptr_eq(server.blocklists(), &mgr));
    }

    #[tokio::test]
    async fn server_binds_unix_socket_with_expected_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let handles = default_handles();
        let path = socket_path(&dir);
        let server = WsServer::new(path.clone(), Token::generate(), handles);
        let _listener = server.bind().await.expect("bind should succeed");

        assert!(path.exists(), "socket file should exist after bind");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket file should be mode 0600");

        let parent_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "parent dir should be mode 0700");
    }

    #[tokio::test]
    async fn connection_without_token_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let token = Token::generate();
        let (path, _broadcast_tx, mut inbound_rx, _join) =
            spawn_server_with_inbound(&dir, token).await;

        let mut ws = connect(&path).await;

        // Send a ClientMessage-shaped frame *without* presenting the token
        // first — this should be consumed as the (failed) handshake attempt
        // and the connection closed, never reaching `handles.inbound`.
        ws.send(TMessage::Text(
            serde_json::json!({
                "action": "setVerdict",
                "rowId": "ask-1",
                "verdict": "allow",
                "scope": "this_host",
                "duration": "once"
            })
            .to_string(),
        ))
        .await
        .expect("send should not fail at the transport level");

        // The server should close the connection right after rejecting the
        // handshake.
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
        match next {
            Ok(Some(Ok(TMessage::Close(_)))) | Ok(None) => {}
            other => panic!("expected connection close after failed handshake, got {other:?}"),
        }

        let got_inbound =
            tokio::time::timeout(std::time::Duration::from_millis(200), inbound_rx.recv()).await;
        assert!(
            got_inbound.is_err(),
            "no ClientMessage should reach handles.inbound without a valid token"
        );
    }

    #[tokio::test]
    async fn connection_with_wrong_token_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let token = Token::generate();
        let (path, _broadcast_tx, mut inbound_rx, _join) =
            spawn_server_with_inbound(&dir, token).await;

        let mut ws = connect(&path).await;
        ws.send(TMessage::Text("definitely-not-the-token".to_string()))
            .await
            .unwrap();

        let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
        match next {
            Ok(Some(Ok(TMessage::Close(_)))) | Ok(None) => {}
            other => panic!("expected connection close after wrong token, got {other:?}"),
        }

        let got_inbound =
            tokio::time::timeout(std::time::Duration::from_millis(200), inbound_rx.recv()).await;
        assert!(
            got_inbound.is_err(),
            "wrong token must not unlock the stream"
        );
    }

    #[tokio::test]
    async fn connection_with_correct_token_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let token = Token::generate();
        let (path, broadcast_tx, mut inbound_rx, _join) =
            spawn_server_with_inbound(&dir, token.clone()).await;

        let mut ws = connect(&path).await;

        // 1. Present the token first.
        ws.send(TMessage::Text(token.as_str().to_string()))
            .await
            .unwrap();

        // The acknowledgement is deliberately the first server frame. A
        // client must wait for this rather than assuming its token write was
        // accepted.
        let acknowledgement = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("should receive authentication acknowledgement")
            .expect("stream should not end")
            .expect("frame should not error");
        assert!(matches!(
            acknowledgement,
            TMessage::Text(ref text)
                if matches!(serde_json::from_str(text), Ok(ServerMessage::Authenticated))
        ));

        // 2. Now a real ClientMessage should reach `handles.inbound`.
        let verdict = serde_json::json!({
            "action": "setVerdict",
            "rowId": "ask-1",
            "verdict": "allow",
            "scope": "this_host",
            "duration": "once"
        });
        ws.send(TMessage::Text(verdict.to_string())).await.unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), inbound_rx.recv())
            .await
            .expect("should receive a ClientMessage")
            .expect("channel should not be closed");
        match received {
            ClientMessage::SetVerdict { row_id, .. } => assert_eq!(row_id, "ask-1"),
            other => panic!("expected SetVerdict, got {other:?}"),
        }

        // 3. Broadcast messages still flow to the client after handshake.
        broadcast_tx
            .send(ServerMessage::InsertConnectionRows { rows: vec![] })
            .unwrap();
        let outbound = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("should receive a broadcast message")
            .expect("stream should not end")
            .expect("frame should not error");
        match outbound {
            TMessage::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                assert_eq!(
                    v.get("action").and_then(|a| a.as_str()),
                    Some("insertConnectionRows")
                );
            }
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    #[cfg(feature = "web-ui")]
    #[tokio::test]
    async fn server_serves_index_html_at_root_after_handshake_token_gate() {
        use axum::body::to_bytes;
        use axum::http::Request;
        use tower::ServiceExt;

        let handles = default_handles();
        let state = AppState {
            handles,
            token: Token::generate(),
        };

        let app = WsServer::router(state);

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

    #[cfg(feature = "web-ui")]
    #[tokio::test]
    async fn server_serves_asset_js_unauthenticated() {
        use axum::http::Request;
        use tower::ServiceExt;

        let handles = default_handles();
        let state = AppState {
            handles,
            token: Token::generate(),
        };
        let app = WsServer::router(state);

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

    /// The release tarball's build (`--no-default-features`): no embedded
    /// frontend, so the static routes and the SPA fallback are all 404s.
    #[cfg(not(feature = "web-ui"))]
    #[tokio::test]
    async fn without_web_ui_static_routes_are_not_served() {
        use axum::http::Request;
        use tower::ServiceExt;

        for uri in ["/", "/assets/js/app.js", "/some/spa/route"] {
            let state = AppState {
                handles: default_handles(),
                token: Token::generate(),
            };
            let response = WsServer::router(state)
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                404,
                "{uri} must not be served without web-ui"
            );
        }
    }
}
