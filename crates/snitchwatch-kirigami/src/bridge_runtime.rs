//! Client runtime for the separately managed Snitchwatch bridge service.
//!
//! The Kirigami process never starts a bridge: it reads the service-owned
//! token, connects to its Unix-domain WebSocket, and forwards the typed stream
//! into the same channels the QML models already consume. This deliberately
//! leaves gRPC binding, socket creation/unlinking, and token-file writes to
//! `snitchwatch-bridge-cli` and the service manager.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use tokio::net::UnixStream;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const AUTHENTICATION_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Cheaply-clonable typed channels for model feeds and UI actions.
#[derive(Clone)]
pub struct BridgeHandles {
    broadcast_tx: broadcast::Sender<ReceivedServerMessage>,
    inbound_tx: mpsc::Sender<QueuedClientMessage>,
    runtime: Handle,
    connection: Arc<Mutex<ConnectionState>>,
}

/// A service message labelled with the authenticated WebSocket session that
/// received it. Qt delivery is asynchronous, so consumers must retain this
/// label until their queued Qt callback runs; a callback from an older service
/// instance must not mutate a model after reconnect.
#[derive(Clone, Debug)]
pub struct ReceivedServerMessage {
    pub connection_id: u64,
    pub message: ServerMessage,
}

/// Session-labelled shell state, subject to the same queued-Qt stale-frame
/// guard as model messages.
#[derive(Clone, Debug)]
pub struct ReceivedTrayState {
    pub connection_id: u64,
    pub state: BridgeTrayState,
}

#[derive(Clone, Debug)]
pub struct ReceivedNotice {
    pub connection_id: u64,
    pub notice: BridgeNotice,
}

struct QueuedClientMessage {
    connection_id: u64,
    message: ClientMessage,
}

#[derive(Default)]
struct ConnectionState {
    connected: bool,
    connection_id: u64,
}

/// Why a UI request was not handed to the currently connected service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendClientMessageError {
    Disconnected,
    Full,
    Stopped,
}

impl std::fmt::Display for SendClientMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disconnected => "bridge disconnected",
            Self::Full => "bridge request queue full",
            Self::Stopped => "bridge client stopped",
        })
    }
}

impl BridgeHandles {
    pub fn subscribe(&self) -> broadcast::Receiver<ReceivedServerMessage> {
        self.broadcast_tx.subscribe()
    }

    /// True only while `connection_id` still identifies the live service
    /// session. This is intentionally checked on the Qt thread, immediately
    /// before a queued model mutation.
    pub fn is_current_session(&self, connection_id: u64) -> bool {
        is_current_connection(&self.connection, connection_id)
    }

    pub fn runtime(&self) -> &Handle {
        &self.runtime
    }

    /// Whether the authenticated WebSocket is live right now. GUI mutations
    /// must use this as a gate instead of being retained for a later service
    /// instance after a disconnect.
    pub fn is_connected(&self) -> bool {
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .connected
    }

    /// Queue a message only for the WebSocket session that is currently live.
    /// A disconnect invalidates its connection id before reconnecting, so an
    /// action accepted just before loss can never be replayed to a replacement
    /// bridge service.
    pub fn try_send(&self, message: ClientMessage) -> Result<(), SendClientMessageError> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !connection.connected {
            return Err(SendClientMessageError::Disconnected);
        }
        self.inbound_tx
            .try_send(QueuedClientMessage {
                connection_id: connection.connection_id,
                message,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SendClientMessageError::Full,
                mpsc::error::TrySendError::Closed(_) => SendClientMessageError::Stopped,
            })
    }
}

struct ClientRuntime {
    handles: BridgeHandles,
    status: Arc<Mutex<String>>,
    tray_tx: watch::Sender<ReceivedTrayState>,
    notice_tx: broadcast::Sender<ReceivedNotice>,
    // Runtime must outlive its reconnect task. The task is intentionally
    // detached: GUI shutdown drops the process and hence this runtime.
    _runtime: Runtime,
}

enum Outcome {
    Running(Box<ClientRuntime>),
    Failed(String),
}

static STARTED: OnceLock<Outcome> = OnceLock::new();

fn socket_path() -> PathBuf {
    std::env::var_os("SNITCHWATCH_WS_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| snitchwatch_bridge::auth::runtime_dir().join("bridge.sock"))
}

fn token_path(socket_path: &std::path::Path) -> PathBuf {
    socket_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("token")
}

fn set_status(status: &Arc<Mutex<String>>, value: impl Into<String>) {
    *status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value.into();
}

fn start_inner() -> anyhow::Result<ClientRuntime> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (broadcast_tx, _) = broadcast::channel(1_024);
    let (inbound_tx, inbound_rx) = mpsc::channel(256);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let (notice_tx, _) = broadcast::channel(64);
    let status = Arc::new(Mutex::new("Connecting to bridge service…".to_string()));
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx: broadcast_tx.clone(),
        inbound_tx,
        runtime: runtime.handle().clone(),
        connection: connection.clone(),
    };
    runtime.handle().spawn(client_loop(
        socket_path(),
        broadcast_tx,
        inbound_rx,
        status.clone(),
        tray_tx.clone(),
        notice_tx.clone(),
        connection,
    ));
    Ok(ClientRuntime {
        handles,
        status,
        tray_tx,
        notice_tx,
        _runtime: runtime,
    })
}

/// Connect once, authenticate, then relay both directions until the service
/// disconnects. The runtime requests a snapshot immediately after every
/// successful authentication, including reconnects; model feeds also request
/// one after subscribing so their initial snapshot cannot be lost before QML
/// is ready.
/// Token reads are read-only;
/// a missing/replaced token is a normal reconnect condition during restarts.
async fn client_loop(
    socket_path: PathBuf,
    broadcast_tx: broadcast::Sender<ReceivedServerMessage>,
    mut inbound_rx: mpsc::Receiver<QueuedClientMessage>,
    status: Arc<Mutex<String>>,
    tray_tx: watch::Sender<ReceivedTrayState>,
    notice_tx: broadcast::Sender<ReceivedNotice>,
    connection: Arc<Mutex<ConnectionState>>,
) {
    loop {
        match connect_and_relay(
            &socket_path,
            &broadcast_tx,
            &mut inbound_rx,
            &status,
            &tray_tx,
            &notice_tx,
            &connection,
        )
        .await
        {
            Ok(()) => {
                disconnect_and_discard(&connection, &mut inbound_rx);
                set_status(&status, "Bridge client stopped");
                return;
            }
            Err(error) => {
                disconnect_and_discard(&connection, &mut inbound_rx);
                set_status(&status, format!("Bridge unavailable: {error}"));
                tracing::warn!(error = %error, socket = %socket_path.display(), "bridge service connection lost; retrying");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

async fn connect_and_relay(
    socket_path: &std::path::Path,
    broadcast_tx: &broadcast::Sender<ReceivedServerMessage>,
    inbound_rx: &mut mpsc::Receiver<QueuedClientMessage>,
    status: &Arc<Mutex<String>>,
    tray_tx: &watch::Sender<ReceivedTrayState>,
    notice_tx: &broadcast::Sender<ReceivedNotice>,
    connection: &Arc<Mutex<ConnectionState>>,
) -> anyhow::Result<()> {
    let token_path = token_path(socket_path);
    let token = snitchwatch_bridge::auth::read_token_file(&token_path)
        .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", token_path.display()))?;
    let stream = UnixStream::connect(socket_path).await?;
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream).await?;
    ws.send(Message::Text(token.as_str().to_owned())).await?;

    // Writing the token only proves that the local socket accepted a frame.
    // Do not expose a connected UI or send operational requests until the
    // service explicitly confirms it accepted this service-generation token.
    // A service restart can otherwise make a stale token look connected until
    // the next read happens to fail.
    await_authentication_ack(&mut ws).await?;

    // The service owns all state. Request it for every freshly authenticated
    // connection so a restart cannot leave models showing the previous
    // service instance's rows until another natural event arrives.
    ws.send(Message::Text(serde_json::to_string(
        &ClientMessage::RequestSnapshot,
    )?))
    .await?;
    let connection_id = mark_connected(connection);
    set_status(status, "Connected to bridge service");
    tracing::info!(socket = %socket_path.display(), "connected to bridge service");

    loop {
        tokio::select! {
            outbound = inbound_rx.recv() => match outbound {
                Some(queued) if is_current_connection(connection, queued.connection_id) => {
                    ws.send(Message::Text(serde_json::to_string(&queued.message)?)).await?
                }
                Some(_) => {
                    tracing::debug!("dropping client message from a stale bridge connection");
                }
                None => return Ok(()),
            },
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let message: ServerMessage = serde_json::from_str(&text)?;
                    forward_shell_message(&message, connection_id, tray_tx, notice_tx);
                    let _ = broadcast_tx.send(ReceivedServerMessage { connection_id, message });
                }
                Some(Ok(Message::Close(_))) | None => anyhow::bail!("bridge closed the WebSocket"),
                Some(Ok(Message::Ping(payload))) => ws.send(Message::Pong(payload)).await?,
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.into()),
            },
        }
    }
}

async fn await_authentication_ack(
    ws: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
) -> anyhow::Result<()> {
    loop {
        let incoming = tokio::time::timeout(AUTHENTICATION_ACK_TIMEOUT, ws.next())
            .await
            .map_err(|_| {
                anyhow::anyhow!("timed out waiting for bridge authentication acknowledgement")
            })?;
        match incoming {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMessage>(&text)? {
                ServerMessage::Authenticated => return Ok(()),
                other => anyhow::bail!(
                    "bridge sent {:?} before authentication acknowledgement",
                    other
                ),
            },
            Some(Ok(Message::Close(_))) | None => {
                anyhow::bail!("bridge closed before authentication acknowledgement")
            }
            Some(Ok(Message::Ping(payload))) => ws.send(Message::Pong(payload)).await?,
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

fn mark_connected(connection: &Mutex<ConnectionState>) -> u64 {
    let mut state = connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.connection_id = state.connection_id.wrapping_add(1);
    state.connected = true;
    state.connection_id
}

fn is_current_connection(connection: &Mutex<ConnectionState>, connection_id: u64) -> bool {
    let state = connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.connected && state.connection_id == connection_id
}

/// Close the UI submission gate before discarding work. Holding the same lock
/// used by `BridgeHandles::try_send` makes checking the connection and
/// enqueueing one atomic operation: messages after this point are rejected,
/// and messages accepted before it are drained rather than replayed.
fn disconnect_and_discard(
    connection: &Mutex<ConnectionState>,
    inbound_rx: &mut mpsc::Receiver<QueuedClientMessage>,
) {
    connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .connected = false;
    while inbound_rx.try_recv().is_ok() {}
}

fn forward_shell_message(
    message: &ServerMessage,
    connection_id: u64,
    tray_tx: &watch::Sender<ReceivedTrayState>,
    notice_tx: &broadcast::Sender<ReceivedNotice>,
) {
    match message {
        ServerMessage::TrayState { state } => {
            let _ = tray_tx.send_replace(ReceivedTrayState {
                connection_id,
                state: state.clone(),
            });
        }
        ServerMessage::Notice { notice } => {
            let _ = notice_tx.send(ReceivedNotice {
                connection_id,
                notice: notice.clone(),
            });
        }
        _ => {}
    }
}

/// Start the client runtime once. This is non-fatal even when the service is
/// down: the reconnect task continues and the UI can display its live status.
pub fn ensure_started() -> (bool, String) {
    let outcome = STARTED.get_or_init(|| match start_inner() {
        Ok(runtime) => Outcome::Running(Box::new(runtime)),
        Err(error) => Outcome::Failed(format!("{error:#}")),
    });
    status_of(outcome)
}

pub fn handles() -> Option<BridgeHandles> {
    match STARTED.get()? {
        Outcome::Running(runtime) => Some(runtime.handles.clone()),
        Outcome::Failed(_) => None,
    }
}

pub fn status() -> Option<(bool, String)> {
    STARTED.get().map(status_of)
}

pub fn tray_rx() -> Option<watch::Receiver<ReceivedTrayState>> {
    match STARTED.get()? {
        Outcome::Running(runtime) => Some(runtime.tray_tx.subscribe()),
        Outcome::Failed(_) => None,
    }
}

pub fn notice_rx() -> Option<broadcast::Receiver<ReceivedNotice>> {
    match STARTED.get()? {
        Outcome::Running(runtime) => Some(runtime.notice_tx.subscribe()),
        Outcome::Failed(_) => None,
    }
}

fn status_of(outcome: &Outcome) -> (bool, String) {
    match outcome {
        Outcome::Running(runtime) => {
            let message = runtime
                .status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            (message == "Connected to bridge service", message)
        }
        Outcome::Failed(message) => (false, format!("Bridge unavailable: {message}")),
    }
}

pub use snitchwatch_bridge::notice::Notice as BridgeNotice;
pub use snitchwatch_bridge::tray_state::TrayState as BridgeTrayState;

#[cfg(test)]
mod tests {
    use super::*;

    async fn accept_authenticated_snapshot(
        listener: &tokio::net::UnixListener,
        expected_token: &snitchwatch_bridge::auth::Token,
    ) {
        let (stream, _) = listener.accept().await.expect("client connects");
        let mut ws = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket upgrade succeeds");
        let presented = ws
            .next()
            .await
            .expect("client sends its token")
            .expect("token frame is valid")
            .into_text()
            .expect("token is a text frame");
        assert!(
            expected_token.matches(&presented),
            "client must re-read the current service token"
        );
        ws.send(Message::Text(
            serde_json::to_string(&ServerMessage::Authenticated).unwrap(),
        ))
        .await
        .expect("authentication acknowledgement writes");
        let snapshot: ClientMessage = serde_json::from_str(
            &ws.next()
                .await
                .expect("client requests a snapshot")
                .expect("snapshot frame is valid")
                .into_text()
                .expect("snapshot is text"),
        )
        .expect("snapshot JSON parses");
        assert_eq!(snapshot, ClientMessage::RequestSnapshot);
    }

    #[tokio::test]
    async fn shell_messages_rehydrate_external_tray_and_notice_feeds() {
        let (tray_tx, mut tray_rx) = watch::channel(ReceivedTrayState {
            connection_id: 0,
            state: BridgeTrayState::Idle,
        });
        let (notice_tx, mut notice_rx) = broadcast::channel(4);

        forward_shell_message(
            &ServerMessage::TrayState {
                state: BridgeTrayState::Pending(2),
            },
            1,
            &tray_tx,
            &notice_tx,
        );
        tray_rx.changed().await.expect("tray sender is alive");
        assert_eq!(tray_rx.borrow().connection_id, 1);
        assert_eq!(tray_rx.borrow().state, BridgeTrayState::Pending(2));

        let notice = BridgeNotice::Pending {
            row_id: 17,
            process: "firefox".into(),
        };
        forward_shell_message(
            &ServerMessage::Notice {
                notice: notice.clone(),
            },
            1,
            &tray_tx,
            &notice_tx,
        );
        let received = notice_rx.recv().await.unwrap();
        assert_eq!(received.connection_id, 1);
        assert_eq!(received.notice, notice);
    }

    #[tokio::test]
    async fn disconnect_discards_queued_actions_and_rejects_new_ones() {
        let (broadcast_tx, _) = broadcast::channel(1);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let connection = Arc::new(Mutex::new(ConnectionState::default()));
        let handles = BridgeHandles {
            broadcast_tx,
            inbound_tx,
            runtime: Handle::current(),
            connection: connection.clone(),
        };

        mark_connected(&connection);
        handles
            .try_send(ClientMessage::RecheckDiagnostics)
            .expect("the live session accepts a recheck");
        disconnect_and_discard(&connection, &mut inbound_rx);

        assert!(!handles.is_connected());
        assert!(
            !handles.is_current_session(1),
            "a queued Qt callback from the disconnected session must be dropped"
        );
        assert!(matches!(
            handles.try_send(ClientMessage::RecheckDiagnostics),
            Err(SendClientMessageError::Disconnected)
        ));
        assert!(matches!(
            inbound_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        // A subsequent bridge session gets its own connection id. The stale
        // diagnostics request above is gone rather than replayed here.
        mark_connected(&connection);
        assert!(handles.is_current_session(2));
        assert!(
            !handles.is_current_session(1),
            "a reconnect must not make the prior session current again"
        );
        handles
            .try_send(ClientMessage::RecheckDiagnostics)
            .expect("the replacement session accepts a fresh recheck");
        assert_eq!(
            inbound_rx
                .recv()
                .await
                .expect("fresh request queued")
                .connection_id,
            2
        );
    }

    #[tokio::test]
    async fn client_loop_forwards_authenticated_snapshot_to_the_qml_feed() {
        let dir = tempfile::tempdir().unwrap();
        let config = snitchwatch_bridge_cli::BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = snitchwatch_bridge_cli::run(config.clone())
            .await
            .expect("bridge starts");
        let (shell_tx, mut shell_messages) = broadcast::channel(8);
        let (inbound_tx, inbound_rx) = mpsc::channel(1);
        let (tray_tx, _) = watch::channel(ReceivedTrayState {
            connection_id: 0,
            state: BridgeTrayState::Idle,
        });
        let (notice_tx, _) = broadcast::channel(1);
        let status = Arc::new(Mutex::new(String::new()));
        let connection = Arc::new(Mutex::new(ConnectionState::default()));
        let client = tokio::spawn(client_loop(
            config.ws_socket_path.clone(),
            shell_tx,
            inbound_rx,
            status,
            tray_tx,
            notice_tx,
            connection.clone(),
        ));

        // This verifies the production external-client path rather than only
        // the bridge's internal broadcast. An empty authenticated service
        // answers RequestSnapshot with messages that reach the QML-facing
        // shell feed.
        let mut saw_clear = false;
        let mut saw_blocklists = false;
        let mut saw_profiles = false;
        let mut saw_tray = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !(saw_clear && saw_blocklists && saw_profiles && saw_tray) {
            match tokio::time::timeout_at(deadline, shell_messages.recv())
                .await
                .expect("client did not forward the authenticated snapshot")
                .expect("QML-facing shell feed closed")
            {
                ReceivedServerMessage {
                    connection_id: 1,
                    message: ServerMessage::ClearConnectionRows,
                } => saw_clear = true,
                ReceivedServerMessage {
                    connection_id: 1,
                    message: ServerMessage::SetBlocklists { .. },
                } => saw_blocklists = true,
                ReceivedServerMessage {
                    connection_id: 1,
                    message: ServerMessage::SetProfiles { .. },
                } => saw_profiles = true,
                ReceivedServerMessage {
                    connection_id: 1,
                    message:
                        ServerMessage::TrayState {
                            state: BridgeTrayState::Idle,
                        },
                } => saw_tray = true,
                _ => {}
            }
        }
        assert!(is_current_connection(&connection, 1));

        drop(inbound_tx);
        client.abort();
        bridge.shutdown();
    }

    #[tokio::test]
    async fn client_stays_pending_until_service_acknowledges_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("bridge.sock");
        let token_path = dir.path().join("token");
        let token = snitchwatch_bridge::auth::Token::generate();
        snitchwatch_bridge::auth::write_token_file(&token, &token_path).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (token_received_tx, token_received_rx) = tokio::sync::oneshot::channel();
        let (release_ack_tx, release_ack_rx) = tokio::sync::oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let presented = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(presented, token.as_str());
            token_received_tx.send(()).unwrap();

            release_ack_rx.await.unwrap();
            ws.send(Message::Text(
                serde_json::to_string(&ServerMessage::Authenticated).unwrap(),
            ))
            .await
            .unwrap();

            let snapshot: ClientMessage =
                serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap())
                    .unwrap();
            assert_eq!(snapshot, ClientMessage::RequestSnapshot);
            std::future::pending::<()>().await;
        });

        let (broadcast_tx, _) = broadcast::channel(4);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        let (tray_tx, _) = watch::channel(ReceivedTrayState {
            connection_id: 0,
            state: BridgeTrayState::Idle,
        });
        let (notice_tx, _) = broadcast::channel(1);
        let status = Arc::new(Mutex::new(String::new()));
        let connection = Arc::new(Mutex::new(ConnectionState::default()));
        let client_connection = connection.clone();
        let client_status = status.clone();
        let client = tokio::spawn(async move {
            connect_and_relay(
                &socket_path,
                &broadcast_tx,
                &mut inbound_rx,
                &client_status,
                &tray_tx,
                &notice_tx,
                &client_connection,
            )
            .await
        });

        token_received_rx.await.unwrap();
        assert!(
            !is_current_connection(&connection, 1),
            "sending the token alone must not expose a connected session"
        );
        assert_ne!(
            status.lock().unwrap().as_str(),
            "Connected to bridge service",
            "the status must remain pending until the acknowledgement arrives"
        );

        release_ack_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if is_current_connection(&connection, 1) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("client should connect after the acknowledgement");

        drop(inbound_tx);
        client.await.unwrap().unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn client_loop_reconnects_after_service_socket_and_token_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("bridge.sock");
        let token_path = dir.path().join("token");
        let first_token = snitchwatch_bridge::auth::Token::generate();
        snitchwatch_bridge::auth::write_token_file(&first_token, &token_path).unwrap();
        let first_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

        let (first_snapshot_tx, first_snapshot_rx) = tokio::sync::oneshot::channel();
        let first_server = tokio::spawn(async move {
            accept_authenticated_snapshot(&first_listener, &first_token).await;
            first_snapshot_tx.send(()).unwrap();
            // Dropping this listener and its WebSocket simulates the service
            // stopping. It deliberately does not leave a usable connection.
        });

        let (broadcast_tx, _) = broadcast::channel(4);
        let (inbound_tx, inbound_rx) = mpsc::channel(1);
        let (tray_tx, _) = watch::channel(ReceivedTrayState {
            connection_id: 0,
            state: BridgeTrayState::Idle,
        });
        let (notice_tx, _) = broadcast::channel(1);
        let status = Arc::new(Mutex::new(String::new()));
        let connection = Arc::new(Mutex::new(ConnectionState::default()));
        let client = tokio::spawn(client_loop(
            socket_path.clone(),
            broadcast_tx,
            inbound_rx,
            status,
            tray_tx,
            notice_tx,
            connection.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(2), first_snapshot_rx)
            .await
            .expect("first service generation receives a snapshot")
            .unwrap();
        first_server.await.unwrap();
        std::fs::remove_file(&socket_path).unwrap();

        let second_token = snitchwatch_bridge::auth::Token::generate();
        snitchwatch_bridge::auth::write_token_file(&second_token, &token_path).unwrap();
        let second_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (second_snapshot_tx, second_snapshot_rx) = tokio::sync::oneshot::channel();
        let second_server = tokio::spawn(async move {
            accept_authenticated_snapshot(&second_listener, &second_token).await;
            second_snapshot_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });

        tokio::time::timeout(Duration::from_secs(3), second_snapshot_rx)
            .await
            .expect("replacement service receives a fresh snapshot")
            .unwrap();
        assert!(is_current_connection(&connection, 2));

        // `client_loop` is intentionally long-lived while its runtime owns
        // it; abort the test task after proving the replacement connection.
        drop(inbound_tx);
        client.abort();
        second_server.abort();
    }

    #[tokio::test]
    async fn client_loop_recovers_from_missing_and_stale_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("bridge.sock");
        let token_path = dir.path().join("token");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let stale_token = snitchwatch_bridge::auth::Token::generate();
        let current_token = snitchwatch_bridge::auth::Token::generate();
        let server_stale_token = stale_token.clone();
        let server_current_token = current_token.clone();
        let (stale_seen_tx, stale_seen_rx) = tokio::sync::oneshot::channel();
        let (recovered_tx, recovered_rx) = tokio::sync::oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stale_ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let stale_presented = stale_ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(server_stale_token.matches(&stale_presented));
            stale_seen_tx.send(()).unwrap();
            // A real service rejects a stale token by closing before its ack.
            stale_ws.close(None).await.unwrap();

            accept_authenticated_snapshot(&listener, &server_current_token).await;
            recovered_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });

        let (broadcast_tx, _) = broadcast::channel(4);
        let (inbound_tx, inbound_rx) = mpsc::channel(1);
        let (tray_tx, _) = watch::channel(ReceivedTrayState {
            connection_id: 0,
            state: BridgeTrayState::Idle,
        });
        let (notice_tx, _) = broadcast::channel(1);
        let status = Arc::new(Mutex::new(String::new()));
        let connection = Arc::new(Mutex::new(ConnectionState::default()));
        let client = tokio::spawn(client_loop(
            socket_path,
            broadcast_tx,
            inbound_rx,
            status,
            tray_tx,
            notice_tx,
            connection,
        ));

        // Let the first attempt observe the absent file. Its next attempt
        // reads this stale generation and is rejected by the service.
        tokio::time::sleep(Duration::from_millis(50)).await;
        snitchwatch_bridge::auth::write_token_file(&stale_token, &token_path).unwrap();
        tokio::time::timeout(Duration::from_secs(3), stale_seen_rx)
            .await
            .expect("client retries once a token appears")
            .unwrap();
        snitchwatch_bridge::auth::write_token_file(&current_token, &token_path).unwrap();

        tokio::time::timeout(Duration::from_secs(3), recovered_rx)
            .await
            .expect("client recovers after token rotation")
            .unwrap();
        drop(inbound_tx);
        client.abort();
        server.abort();
    }

    #[test]
    fn production_client_runtime_cannot_take_over_service_resources() {
        // Keep this narrow and intentional: test only the production section,
        // so test fixtures may still start a bridge service in-process.
        let production = include_str!("bridge_runtime.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "snitchwatch_bridge_cli::run(",
            "UnixListener::bind(",
            "write_token_file(",
            "remove_file(",
            "127.0.0.1:50051",
        ] {
            assert!(
                !production.contains(forbidden),
                "external Kirigami client must not contain {forbidden}"
            );
        }
        let entrypoint = include_str!("main.rs");
        assert!(
            !entrypoint.contains("snitchwatch_bridge_cli"),
            "the production binary must only start the external client runtime"
        );
    }
}
