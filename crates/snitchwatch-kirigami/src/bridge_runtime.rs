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
const SYSTEM_SOCKET_PATH: &str = "/run/snitchwatch/bridge.sock";
const SYSTEM_TOKEN_PATH: &str = "/run/snitchwatch-auth/token";

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

/// Session-labelled filter-pause state (issue #47), routed and guarded the
/// same way as [`ReceivedTrayState`].
#[derive(Clone, Debug)]
pub struct ReceivedPauseState {
    pub connection_id: u64,
    pub state: BridgePauseState,
}

#[derive(Clone, Debug)]
pub struct ReceivedNotice {
    pub connection_id: u64,
    pub notice: BridgeNotice,
}

/// Senders for the shell state the runtime forwards next to the model feed:
/// tray state, desktop notices and filter-pause state.
struct ShellFeeds {
    tray_tx: watch::Sender<ReceivedTrayState>,
    notice_tx: broadcast::Sender<ReceivedNotice>,
    pause_tx: watch::Sender<ReceivedPauseState>,
}

struct QueuedClientMessage {
    connection_id: u64,
    message: ClientMessage,
}

#[derive(Default)]
struct ConnectionState {
    connected: bool,
    connection_id: u64,
    /// Whether this connection's bridge advertised
    /// `bridge_capabilities::APP_BOUND_RULES` in its acknowledgement. Set
    /// with `connection_id`, cleared on disconnect.
    app_bound_rules: bool,
}

/// Why a UI request was not handed to the currently connected service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendClientMessageError {
    Disconnected,
    StaleSession,
    Full,
    Stopped,
}

impl std::fmt::Display for SendClientMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disconnected => "bridge disconnected",
            Self::StaleSession => "action belongs to an older bridge session",
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

    /// True only while `connection_id` is the live session and its bridge
    /// advertised app-bound rules. The inline Deny is remembered only then
    /// (`crate::inline_deny`).
    pub fn advertises_app_bound_rules(&self, connection_id: u64) -> bool {
        advertises_app_bound_rules(&self.connection, connection_id)
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
        self.try_send_with_session(None, message)
    }

    /// Preserve the originating row's session through the final queue admission.
    /// Checking and enqueueing under the same lock closes the reconnect race.
    pub fn try_send_for_session(
        &self,
        connection_id: u64,
        message: ClientMessage,
    ) -> Result<(), SendClientMessageError> {
        self.try_send_with_session(Some(connection_id), message)
    }

    fn try_send_with_session(
        &self,
        expected_session: Option<u64>,
        message: ClientMessage,
    ) -> Result<(), SendClientMessageError> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !connection.connected {
            return Err(SendClientMessageError::Disconnected);
        }
        if expected_session.is_some_and(|id| id != connection.connection_id) {
            return Err(SendClientMessageError::StaleSession);
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
    pause_tx: watch::Sender<ReceivedPauseState>,
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
    resolve_socket_path(
        std::env::var_os("SNITCHWATCH_WS_SOCKET"),
        std::env::var_os("SNITCHWATCH_SYSTEM_BRIDGE").as_deref() == Some(std::ffi::OsStr::new("1")),
        &snitchwatch_bridge::auth::runtime_dir(),
    )
}

fn resolve_socket_path(
    override_path: Option<std::ffi::OsString>,
    system_mode: bool,
    legacy_dir: &std::path::Path,
) -> PathBuf {
    match override_path {
        Some(path) => PathBuf::from(path),
        None if system_mode => PathBuf::from(SYSTEM_SOCKET_PATH),
        None => legacy_dir.join("bridge.sock"),
    }
}

fn token_path(socket_path: &std::path::Path) -> PathBuf {
    resolve_token_path(socket_path, std::env::var_os("SNITCHWATCH_WS_TOKEN_PATH"))
}

fn resolve_token_path(
    socket_path: &std::path::Path,
    override_path: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(path) = override_path {
        return PathBuf::from(path);
    }
    if socket_path == std::path::Path::new(SYSTEM_SOCKET_PATH) {
        return PathBuf::from(SYSTEM_TOKEN_PATH);
    }
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
    let (pause_tx, _) = watch::channel(ReceivedPauseState {
        connection_id: 0,
        state: BridgePauseState::NOT_PAUSED,
    });
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
        ShellFeeds {
            tray_tx: tray_tx.clone(),
            notice_tx: notice_tx.clone(),
            pause_tx: pause_tx.clone(),
        },
        connection,
    ));
    Ok(ClientRuntime {
        handles,
        status,
        tray_tx,
        notice_tx,
        pause_tx,
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
    shell: ShellFeeds,
    connection: Arc<Mutex<ConnectionState>>,
) {
    loop {
        match connect_and_relay(
            &socket_path,
            &broadcast_tx,
            &mut inbound_rx,
            &status,
            &shell,
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
    shell: &ShellFeeds,
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
    let capabilities = await_authentication_ack(&mut ws).await?;

    // The service owns all state. Request it for every freshly authenticated
    // connection so a restart cannot leave models showing the previous
    // service instance's rows until another natural event arrives.
    ws.send(Message::Text(serde_json::to_string(
        &ClientMessage::RequestSnapshot,
    )?))
    .await?;
    let connection_id = mark_connected(
        connection,
        capabilities
            .iter()
            .any(|c| c == snitchwatch_bridge::bridge_capabilities::APP_BOUND_RULES),
    );
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
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(message) => {
                        forward_shell_message(
                            &message,
                            connection_id,
                            &shell.tray_tx,
                            &shell.notice_tx,
                            &shell.pause_tx,
                        );
                        let _ = broadcast_tx.send(ReceivedServerMessage { connection_id, message });
                    }
                    // A newer bridge may send actions this client doesn't
                    // know. Skip them: dropping the connection would reconnect
                    // forever, cancelling pending prompts each time.
                    Err(error) => {
                        tracing::warn!(%error, "skipping a bridge message this client can't parse");
                    }
                },
                Some(Ok(Message::Close(_))) | None => anyhow::bail!("bridge closed the WebSocket"),
                Some(Ok(Message::Ping(payload))) => ws.send(Message::Pong(payload)).await?,
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.into()),
            },
        }
    }
}

/// Returns the capabilities the bridge advertised (empty for bridges that
/// predate them).
async fn await_authentication_ack(
    ws: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
) -> anyhow::Result<Vec<String>> {
    loop {
        let incoming = tokio::time::timeout(AUTHENTICATION_ACK_TIMEOUT, ws.next())
            .await
            .map_err(|_| {
                anyhow::anyhow!("timed out waiting for bridge authentication acknowledgement")
            })?;
        match incoming {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMessage>(&text)? {
                ServerMessage::Authenticated { capabilities } => return Ok(capabilities),
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

fn mark_connected(connection: &Mutex<ConnectionState>, app_bound_rules: bool) -> u64 {
    let mut state = connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.connection_id = state.connection_id.wrapping_add(1);
    state.connected = true;
    state.app_bound_rules = app_bound_rules;
    state.connection_id
}

/// Whether `connection_id` is the live session and its bridge advertised
/// app-bound rules (inline-Deny plan). Read under the same lock that bumps the
/// id, so a reconnect to an older bridge never inherits the flag.
fn advertises_app_bound_rules(connection: &Mutex<ConnectionState>, connection_id: u64) -> bool {
    let state = connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.connected && state.connection_id == connection_id && state.app_bound_rules
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
    {
        let mut state = connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.connected = false;
        state.app_bound_rules = false;
    }
    while inbound_rx.try_recv().is_ok() {}
}

fn forward_shell_message(
    message: &ServerMessage,
    connection_id: u64,
    tray_tx: &watch::Sender<ReceivedTrayState>,
    notice_tx: &broadcast::Sender<ReceivedNotice>,
    pause_tx: &watch::Sender<ReceivedPauseState>,
) {
    match message {
        ServerMessage::FilterPauseState {
            paused,
            expires_at_unix_ms,
        } => {
            let _ = pause_tx.send_replace(ReceivedPauseState {
                connection_id,
                state: BridgePauseState {
                    paused: *paused,
                    expires_at_unix_ms: *expires_at_unix_ms,
                },
            });
        }
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

pub fn pause_rx() -> Option<watch::Receiver<ReceivedPauseState>> {
    match STARTED.get()? {
        Outcome::Running(runtime) => Some(runtime.pause_tx.subscribe()),
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

pub use snitchwatch_bridge::filter_pause::PauseState as BridgePauseState;
pub use snitchwatch_bridge::notice::Notice as BridgeNotice;
pub use snitchwatch_bridge::tray_state::TrayState as BridgeTrayState;

#[cfg(test)]
#[path = "bridge_runtime/tests.rs"]
mod tests;
