//! Bridge orchestrator — testable library entry point.
//!
//! This crate's binary (`main.rs`) is a thin wrapper around [`run`]. Tests
//! construct a [`BridgeConfig`] and call `run` directly so they don't have
//! to spawn the CLI as a subprocess.
//!
//! What `run` wires together:
//!
//! 1. Binds the WebSocket server on the Unix domain socket at
//!    `ws_socket_path` and writes a fresh handshake token alongside it.
//! 2. Binds the gRPC `Ui` server on `grpc_bind` — opensnitchd dials in here.
//! 3. Inbound `AskRule` RPCs insert a pending row into the cache, broadcast
//!    it on the WebSocket, and await a `oneshot<Verdict>` from the WS layer.
//! 4. Inbound WebSocket `ClientMessage`s go through `upstream::apply`, which
//!    mutates the cache (resolving pending rows by firing the oneshot).

pub mod activation;
mod busy;
pub mod cli;
mod profile_actions;
pub mod profile_storage;
mod replier;
mod rule_commands;
pub mod rule_hits_storage;
mod rules_import;
pub mod storage;
#[cfg(test)]
mod test_daemon;

pub use storage::{
    resolve_storage, BridgeMode, EphemeralReason, RunOptions, Storage, PER_USER_REASON,
};

use anyhow::{Context, Result};
use snitchwatch_bridge::auth::{self, Token};
use snitchwatch_bridge::blocklists::worker::{BlocklistTasks, DEFAULT_REFRESH_TICK};
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::cache::rule_hits_handle::RuleHitsHandle;
use snitchwatch_bridge::cache::rules::{
    prune_expired_rules_every, publish_rules, settle_rule_command,
};
use snitchwatch_bridge::cache::traffic_tracker::TrafficTracker;
use snitchwatch_bridge::daemon_commands::DaemonTransport;
use snitchwatch_bridge::deferred_answers::ANSWER_TIMEOUT;
use snitchwatch_bridge::filter_pause::{FilterPause, PauseRequest};
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::notice::{Notice, NoticeBus};
use snitchwatch_bridge::profiles::network_watcher;
use snitchwatch_bridge::translator::downstream;
use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
use snitchwatch_bridge::translator::upstream::{self, UpstreamEffect};
use snitchwatch_bridge::tray_state::{TrayState, TrayStatePublisher};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};
use tonic::transport::Server;
use tracing::{error, info, warn};

/// Rolling window kept by the traffic pump's [`TrafficTracker`], matching
/// `snitchwatch-kirigami::traffic::ring_store::DEFAULT_WINDOW_SECONDS` (the
/// consumer side of the same underlying `TrafficBinner`).
const TRAFFIC_WINDOW_SECONDS: usize = 300;

/// How long a rule command waits for the daemon's reply before the GUI's
/// optimistic change is rolled back (#48).
const RULE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// How often expired temporary rules are pruned from the rules cache (#48).
const RULE_EXPIRY_TICK: Duration = Duration::from_secs(30);

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

/// Check kernel credentials before tonic sees a daemon connection. Rejected
/// clients are dropped and accepting continues, including after lookup errors.
struct RootUnixIncoming(UnixListener);

impl tokio_stream::Stream for RootUnixIncoming {
    type Item = std::io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        // Bound each poll so a flood of rejected clients cannot starve shutdown.
        for _ in 0..32 {
            match self.0.poll_accept(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(Ok((stream, _))) => match stream.peer_cred() {
                    Ok(cred) if cred.uid() == 0 => return Poll::Ready(Some(Ok(stream))),
                    Ok(cred) => warn!(uid = cred.uid(), "rejected non-root daemon peer"),
                    Err(error) => {
                        warn!(%error, "rejected daemon peer with unavailable credentials")
                    }
                },
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// True for every `ClientMessage` variant `ProfilesManager` owns handling of.
/// Kept as a free function (rather than inlined into the pump's `match`) so
/// it reads as one clear routing decision at the call site.
fn is_profile_message(msg: &ClientMessage) -> bool {
    matches!(
        msg,
        ClientMessage::CreateProfile { .. }
            | ClientMessage::UpdateProfile { .. }
            | ClientMessage::DeleteProfile { .. }
            | ClientMessage::ActivateProfile { .. }
            | ClientMessage::DeactivateProfile
            | ClientMessage::AddProfileRule { .. }
            | ClientMessage::RemoveProfileRule { .. }
    )
}

/// After any pause change, show the current pause state everywhere: on the
/// tray (through the cache, which publishes `FilterOff` while a pause is
/// active) and to every GUI (`FilterPauseState`, issue #47). Read and sent
/// under the cache lock, so announcements from the pump, the expiry task and
/// the last-loss clear reach GUIs in the order they happened, and the last
/// one always matches the last change.
async fn announce_pause_state(
    filter_pause: &FilterPause,
    cache: &Mutex<ConnectionCache>,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
) {
    let cache = cache.lock().await;
    cache.resync_tray_state();
    let _ = broadcast_tx.send(filter_pause.state().to_message());
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

/// Handle to a running bridge. Dropping this does **not** shut the bridge
/// down — call [`RunningBridge::shutdown`] explicitly when you're done.
pub struct RunningBridge {
    /// Unix domain socket path the WS server is listening on.
    pub ws_socket_path: PathBuf,
    /// Token file path (mode 0600 in legacy mode, 0640 in system mode).
    pub ws_token_path: PathBuf,
    /// The handshake token itself, so in-process callers (e.g. the Tauri
    /// shell, tests) don't have to re-read it from disk.
    pub ws_token: Token,
    /// Actual daemon endpoint (TCP in legacy mode, Unix in system mode).
    pub grpc_endpoint: GrpcEndpoint,
    /// Outbound `ServerMessage` broadcast sender. In-process consumers (the
    /// native Kirigami shell) call `.subscribe()` here to receive the exact
    /// stream the WebSocket server fans out to browser clients — no WS
    /// round-trip to ourselves. The WS server keeps using its own clone of this
    /// same sender, so both consumption paths stay in lockstep.
    pub broadcast_tx: broadcast::Sender<ServerMessage>,
    /// Inbound `ClientMessage` sender. In-process consumers push UI-origin
    /// messages here — the same channel the WebSocket server feeds — so they
    /// flow through the identical `upstream::apply` pump (verdict resolution,
    /// rule effects). This is the in-process equivalent of a WS client frame.
    pub inbound_tx: mpsc::Sender<ClientMessage>,
    /// Receiver for tray icon state changes published by the bridge.
    pub tray_rx: watch::Receiver<TrayState>,
    /// Receiver for desktop notifications published by the bridge.
    pub notice_rx: broadcast::Receiver<Notice>,
    /// See [`RunningBridge::daemon_stream_ready`].
    daemon_stream_ready: watch::Receiver<u64>,
    /// Authenticated GUI sessions. The WS server registers each client after
    /// its handshake; tests register one directly to exercise GUI-gated paths
    /// such as prompts and pausing. Test-only so no caller can hold a lease
    /// that leaves the bridge permanently "attended".
    #[cfg(test)]
    client_presence: snitchwatch_bridge::client_presence::ClientPresence,
    ws_shutdown_tx: Option<oneshot::Sender<()>>,
    grpc_shutdown_tx: Option<oneshot::Sender<()>>,
    /// The daemon-down watchdog task (`daemon_watchdog::run`). It has no
    /// external state to flush on stop — unlike the WS/gRPC servers, an
    /// abort is sufficient rather than a graceful oneshot handshake.
    watchdog_handle: tokio::task::JoinHandle<()>,
    /// The filter-pause expiry task (`filter_pause::expire_pause_on_deadline`).
    /// Like the watchdog, it never ends on its own.
    pause_expiry_handle: tokio::task::JoinHandle<()>,
    /// The last-loss pause clear (`clear_pause_on_last_session_loss`). It
    /// holds the cache and broadcast sender, so stop it with the bridge.
    pause_clear_handle: tokio::task::JoinHandle<()>,
    /// The blocklist worker, its refresh loop and event pump (issue #45).
    blocklist_tasks: BlocklistTasks,
    /// The profile enforcer and auto-switch tasks (issue #46).
    profile_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Per-rule hit counts, saved once more at shutdown, and the ticker that
    /// broadcasts and saves them in between.
    rule_hits: RuleHitsHandle,
    rule_hits_ticker: tokio::task::JoinHandle<()>,
}

impl RunningBridge {
    /// Generation bumped each time a daemon `Notifications` stream says HELLO
    /// and becomes the one rule commands are correlated with. Wait with a
    /// level check, `wait_for(|g| *g >= 1)`: `changed()` hangs when the HELLO
    /// was handled before the receiver was taken.
    pub fn daemon_stream_ready(&self) -> watch::Receiver<u64> {
        self.daemon_stream_ready.clone()
    }

    /// Signal every background task to stop. Safe to call more than once.
    pub fn shutdown(mut self) {
        self.watchdog_handle.abort();
        self.pause_expiry_handle.abort();
        self.pause_clear_handle.abort();
        self.blocklist_tasks.abort();
        for task in &self.profile_tasks {
            task.abort();
        }
        // The ticker first, so no save is started behind this one; `save_now`
        // waits for one already running.
        self.rule_hits_ticker.abort();
        self.rule_hits.save_now();
        if let Some(tx) = self.ws_shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.grpc_shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Start the bridge and return as soon as every background task is running.
///
/// Both the WebSocket server and the gRPC `Ui` server are bound and accepting
/// connections by the time this returns. opensnitchd can dial in immediately.
///
/// For in-process callers (tests, the Tauri shell): blocklists are never
/// persisted ([`RunOptions::in_process`]).
pub async fn run(config: BridgeConfig) -> Result<RunningBridge> {
    run_with_options(config, RunOptions::in_process()).await
}

/// [`run`] with explicit [`RunOptions`]: `main.rs` passes the resolved state
/// directory; tests may also inject a blocklist fetcher.
pub async fn run_with_options(config: BridgeConfig, options: RunOptions) -> Result<RunningBridge> {
    run_tcp(config, options, ANSWER_TIMEOUT).await
}

/// [`run`] with a shorter wait before the bridge answers a prompt nobody
/// answers (`snitchwatch_bridge::deferred_answers`), for tests that watch
/// one time out.
pub async fn run_with_answer_timeout(
    config: BridgeConfig,
    answer_timeout: std::time::Duration,
) -> Result<RunningBridge> {
    run_tcp(config, RunOptions::in_process(), answer_timeout).await
}

async fn run_tcp(
    config: BridgeConfig,
    options: RunOptions,
    answer_timeout: std::time::Duration,
) -> Result<RunningBridge> {
    let grpc_listener = tokio::net::TcpListener::bind(config.grpc_bind)
        .await
        .with_context(|| format!("failed to bind gRPC listener on {}", config.grpc_bind))?;
    let endpoint = GrpcEndpoint::Tcp(grpc_listener.local_addr()?);
    run_with_incoming(
        config,
        endpoint,
        tokio_stream::wrappers::TcpListenerStream::new(grpc_listener),
        None,
        None,
        options,
        answer_timeout,
    )
    .await
}

/// Start exclusively on service-manager-owned sockets. Never binds, removes,
/// or changes permissions on either socket or its parent directory.
pub async fn run_system(listeners: activation::ActivatedListeners) -> Result<RunningBridge> {
    activation::validate_paths(&listeners)?;
    let config = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().expect("literal address"),
        ws_socket_path: PathBuf::from(activation::GUI_SOCKET_PATH),
        cache_capacity: 10_000,
    };
    run_with_incoming(
        config,
        GrpcEndpoint::Unix(PathBuf::from(activation::GRPC_SOCKET_PATH)),
        RootUnixIncoming(listeners.grpc),
        Some(listeners.gui),
        Some(PathBuf::from(activation::TOKEN_PATH)),
        RunOptions {
            storage: resolve_storage(BridgeMode::System),
            blocklist_fetcher: None,
            mode: BridgeMode::System,
        },
        ANSWER_TIMEOUT,
    )
    .await
}

async fn run_with_incoming<I, IO>(
    config: BridgeConfig,
    grpc_endpoint: GrpcEndpoint,
    incoming: I,
    activated_ws: Option<UnixListener>,
    system_token_path: Option<PathBuf>,
    options: RunOptions,
    answer_timeout: std::time::Duration,
) -> Result<RunningBridge>
where
    I: tokio_stream::Stream<Item = std::io::Result<IO>> + Send + 'static,
    IO: tokio::io::AsyncRead
        + tokio::io::AsyncWrite
        + tonic::transport::server::Connected
        + Unpin
        + Send
        + 'static,
    IO::ConnectInfo: Clone + Send + Sync + 'static,
{
    info!(?grpc_endpoint, ws_socket = %config.ws_socket_path.display(), cache_capacity = config.cache_capacity, "starting snitchwatch-bridge");

    // Channels between the WS server and the orchestrator.
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(256);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(256);

    // Tray-state publisher, constructed up front so the cache can wire its
    // pending-count transitions into it (see `with_tray_publisher` below) —
    // `TrayStatePublisher::set()` is otherwise never called in production,
    // which left the tray icon stuck at `Idle` regardless of real state.
    let tray_pub = Arc::new(TrayStatePublisher::new());

    // Shared connection cache (pending-row state + decided-row history).
    // `with_tray_publisher` (not `::new`) so `insert_pending`/`resolve`
    // republish `TrayState::Pending(n)`/`Idle` on every change — see
    // `cache::connections`'s `tray_state_tests` module for the existing
    // coverage this wiring already had, just never used in production.
    let filter_pause = Arc::new(FilterPause::new());
    let cache = Arc::new(Mutex::new(
        ConnectionCache::with_tray_publisher(config.cache_capacity, tray_pub.clone())
            .with_filter_pause(filter_pause.clone()),
    ));

    // The daemon side comes first: the blocklist sink sends through its rule
    // commands (#45). Nothing here spawns or serves yet.
    let client_presence = snitchwatch_bridge::client_presence::ClientPresence::default();
    let notice_bus = Arc::new(NoticeBus::new());
    let ui_service_inner = UiService::new(
        cache.clone(),
        broadcast_tx.clone(),
        tray_pub.clone(),
        notice_bus.clone(),
        filter_pause.clone(),
    )
    .with_client_presence(client_presence.clone())
    .with_answer_timeout(answer_timeout)
    .with_daemon_transport(match grpc_endpoint {
        GrpcEndpoint::Tcp(_) => DaemonTransport::Tcp,
        GrpcEndpoint::Unix(_) => DaemonTransport::Unix,
    });
    // Taken before the gRPC server starts, so no rules snapshot is missed.
    let rules_synced = ui_service_inner.rules_synced();
    let profile_rules_synced = ui_service_inner.rules_synced();

    // --- BlocklistsManager: persisted and enforced only when `Persistent` ---
    // The profile store opens in the same state directory but tracks its
    // own storage status (issue #46 Part 1).
    let profiles_storage = options.storage.clone();
    let profiles_mode = options.mode;
    let rule_hits = ui_service_inner.rule_hits_handle();
    rule_hits_storage::configure(&rule_hits, &options.storage);
    let rule_hits_ticker = rule_hits.spawn_ticker();
    let daemon_rules = storage::DaemonRules {
        commands: ui_service_inner.daemon_commands(),
        rules: ui_service_inner.rules_handle(),
    };
    let blocklists_mgr = storage::build_blocklists_manager(options, daemon_rules)?;

    // --- ProfilesManager: persisted when `Persistent`, enforced only by the
    // system bridge (issue #46 Part 2) ---
    let profiles_mgr = profile_storage::build_profiles_manager(
        profiles_storage,
        profiles_mode,
        storage::DaemonRules {
            commands: ui_service_inner.daemon_commands(),
            rules: ui_service_inner.rules_handle(),
        },
    )?;
    let mut profile_tasks = profiles_mgr
        .clone()
        .spawn_enforcer(Some(profile_rules_synced));

    // Network-driven auto-activation. `connect_watcher` degrades to a no-op
    // watcher (manual-activation-only) if NetworkManager/D-Bus isn't
    // reachable — never fails `run`, never panics.
    let network_watcher = network_watcher::connect_watcher().await;
    profile_tasks.push(profiles_mgr.clone().spawn_auto_switch(network_watcher));

    // --- WebSocket server ---------------------------------------------------
    // Generate a fresh handshake token and write it to a file alongside the
    // socket (see `snitchwatch_bridge::auth` for why this is a file, not an
    // env var: a Flatpak-sandboxed GUI client won't share this process's
    // environment, but can read a file under the same
    // `$XDG_RUNTIME_DIR/snitchwatch/` the socket lives under).
    let token = Token::generate();
    let ws_token_path = system_token_path.clone().unwrap_or_else(|| {
        config
            .ws_socket_path
            .parent()
            .map(|p| p.join("token"))
            .unwrap_or_else(|| PathBuf::from("token"))
    });
    if system_token_path.is_some() {
        auth::write_system_token_file(&token, &ws_token_path)
            .context("failed to write system token file")?;
    } else {
        auth::write_token_file(&token, &ws_token_path).context("failed to write token file")?;
    }

    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        presence: client_presence.clone(),
        inbound: inbound_tx.clone(),
        blocklists: blocklists_mgr.clone(),
        profiles: profiles_mgr.clone(),
    };
    let ws_server = WsServer::new(config.ws_socket_path.clone(), token.clone(), ws_handles);
    let ws_listener = match activated_ws {
        Some(listener) => listener,
        None => ws_server
            .bind()
            .await
            .context("failed to bind WebSocket unix socket")?,
    };
    // Spawned after the last fallible step, so a failed start leaves no
    // blocklist worker or refresh loop running.
    let blocklist_tasks = BlocklistTasks::spawn(
        blocklists_mgr.clone(),
        broadcast_tx.clone(),
        DEFAULT_REFRESH_TICK,
        Some(rules_synced),
    );
    let (ws_shutdown_tx, ws_shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            res = ws_server.serve(ws_listener) => {
                if let Err(e) = res {
                    error!(error = %e, "ws_server::serve exited");
                }
            }
            _ = ws_shutdown_rx => {
                info!("ws server shutdown signal received");
            }
        }
    });

    // --- gRPC Ui server -----------------------------------------------------
    let tray_rx = tray_pub.subscribe();
    let notice_rx = notice_bus.subscribe();

    // External shells receive the same tray and desktop-notice inputs as
    // in-process shells. These are additive WebSocket actions, so older
    // clients remain compatible by ignoring actions they do not understand.
    // Subscribe before spawning the pumps; snapshots below cover the current
    // tray value for a client that connects after a state transition.
    {
        let mut tray_events = tray_pub.subscribe();
        let tray_events_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            while tray_events.changed().await.is_ok() {
                let _ = tray_events_tx.send(ServerMessage::TrayState {
                    state: tray_events.borrow().clone(),
                });
            }
        });
    }
    {
        let mut notice_events = notice_bus.subscribe();
        let notice_events_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            loop {
                match notice_events.recv().await {
                    Ok(notice) => {
                        let _ = notice_events_tx.send(ServerMessage::Notice { notice });
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "notice relay lagged behind bridge");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // The filtering pause (issue #47) resets to unpaused on every bridge
    // start, matching every other in-memory bridge state. It ends at the
    // earliest of its deadline, an explicit resume, or the last
    // authenticated GUI session ending.
    let pause_clear_handle = {
        let filter_pause = filter_pause.clone();
        let cache = cache.clone();
        let broadcast_tx = broadcast_tx.clone();
        tokio::spawn(
            snitchwatch_bridge::client_presence::clear_pause_on_last_session_loss(
                client_presence.session_losses(),
                filter_pause.clone(),
                move || {
                    let filter_pause = filter_pause.clone();
                    let cache = cache.clone();
                    let broadcast_tx = broadcast_tx.clone();
                    async move { announce_pause_state(&filter_pause, &cache, &broadcast_tx).await }
                },
            ),
        )
    };
    let pause_expiry_handle = {
        let filter_pause = filter_pause.clone();
        let cache = cache.clone();
        let broadcast_tx = broadcast_tx.clone();
        let notice_bus = notice_bus.clone();
        tokio::spawn(snitchwatch_bridge::filter_pause::expire_pause_on_deadline(
            filter_pause.clone(),
            move || {
                notice_bus.send(Notice::FilterPauseExpired);
                let filter_pause = filter_pause.clone();
                let cache = cache.clone();
                let broadcast_tx = broadcast_tx.clone();
                async move { announce_pause_state(&filter_pause, &cache, &broadcast_tx).await }
            },
        ))
    };

    // Grabbed before `.into_server()` consumes `ui_service_inner` — the
    // daemon-down watchdog below needs this to watch daemon liveness.
    let liveness = ui_service_inner.liveness_handle();

    // Outbound rule commands (enable/disable/delete) for the connected daemon,
    // and the bridge's copy of its rules (#48). Same "grab before
    // into_server()" reason as `liveness` above.
    let daemon_commands = ui_service_inner.daemon_commands();
    let daemon_stream_ready = daemon_commands.stream_ready();
    let rules = ui_service_inner.rules_handle();
    // Names a rule command or an import is changing; neither may race the
    // other on one name (P2.1).
    let busy_names = busy::BusyNames::default();
    // Rule import/export (roadmap P2.7): its own task; the pump only routes.
    let rules_import = rules_import::RulesImport::spawn(
        daemon_commands.clone(),
        rules.clone(),
        broadcast_tx.clone(),
        busy_names.clone(),
    );
    // Rule commands (P2.1 editor checks and results); the pump only routes.
    let rule_commands = rule_commands::RuleCommands::new(
        daemon_commands.clone(),
        rules.clone(),
        broadcast_tx.clone(),
        busy_names.clone(),
    );
    tokio::spawn(prune_expired_rules_every(
        RULE_EXPIRY_TICK,
        Arc::downgrade(&rules),
        broadcast_tx.clone(),
        ui_service_inner.rule_hits_handle(),
    ));

    // Diagnostics: combines daemon-reachability (`liveness`), opensnitchd's
    // reported firewall status, and local kernel probes into the four-check
    // report the GUI renders. Constructed here (before `.into_server()`
    // consumes `ui_service_inner`) so `firewall_status_handle()` is still
    // reachable.
    let firewall_status = ui_service_inner.firewall_status_handle();
    let alert_store = ui_service_inner.alert_store_handle();
    let kernel_probe: Arc<dyn snitchwatch_bridge::diagnostics::kernel_probe::KernelProbe> =
        Arc::new(snitchwatch_bridge::diagnostics::kernel_probe::RealKernelProbe);
    let diagnostics_ctx = Arc::new(snitchwatch_bridge::diagnostics::DiagnosticsCtx::new(
        liveness.clone(),
        firewall_status,
        kernel_probe,
        alert_store,
    ));
    // Late-bind the assembler into `UiService` so `post_alert` can push a
    // fresh report the moment a daemon alert arrives, not just on the next
    // poll/recheck — see `UiService::diagnostics_ctx`'s doc comment for why
    // this can't be a constructor parameter.
    ui_service_inner.set_diagnostics_ctx(diagnostics_ctx.clone());
    // No startup broadcast here: no client has subscribed to `broadcast_tx`
    // yet at this point in `run()`, so a send would always be dropped. The
    // GUI's `DaemonHealthModel::start_bridge_feed` sends
    // `ClientMessage::RecheckDiagnostics` immediately after subscribing,
    // which is the actual startup-report delivery path.

    // Same "grab before into_server()" reason: the snapshot answer.
    let prompt_slot_for_pump = ui_service_inner.prompt_slot_handle();
    let daemon_config_for_pump = ui_service_inner.daemon_config_handle();
    let rule_hits_for_pump = rule_hits.clone();
    let ui_service = ui_service_inner.into_server();
    let (grpc_shutdown_tx, grpc_shutdown_rx) = oneshot::channel::<()>();

    // Daemon-down watchdog: republishes TrayState::DaemonDown when opensnitchd
    // goes unreachable (no gRPC activity and no open Notifications stream),
    // and resyncs to the cache's real Idle/Pending(n) once it's reachable
    // again. See daemon_watchdog's module doc for the timeout rationale.
    let watchdog_handle = tokio::spawn(snitchwatch_bridge::daemon_watchdog::run(
        liveness,
        tray_pub.clone(),
        cache.clone(),
        diagnostics_ctx.clone(),
        broadcast_tx.clone(),
    ));

    tokio::spawn(async move {
        let serve = Server::builder()
            // Dead-peer detection: if the TCP connection to opensnitchd dies
            // without a clean FIN/RST (network drop, host crash, VM pause),
            // a still-pending Notifications stream would otherwise sit open
            // forever, wedging DaemonLiveness::open_notification_streams
            // above zero and making the daemon read as permanently alive.
            // HTTP/2 PING frames every 5s (with a 10s reply timeout) surface
            // that as a real stream close well inside DAEMON_DOWN_TIMEOUT
            // (10s) — see `daemon_liveness`'s module doc for the liveness
            // model this closes the loop on.
            .http2_keepalive_interval(Some(Duration::from_secs(5)))
            .http2_keepalive_timeout(Some(Duration::from_secs(10)))
            .add_service(ui_service)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = grpc_shutdown_rx.await;
            });
        if let Err(e) = serve.await {
            error!(error = %e, "grpc Ui server exited");
        } else {
            info!("grpc Ui server shutdown signal received");
        }
    });

    // --- Profile events → SetProfiles / ProfileChanged broadcasts -----------
    // Mirrors `snitchwatch_bridge::blocklists::spawn_event_pump`: the
    // manager owns no knowledge of the WS wire format, so this is where its
    // internal `ProfileEvent`s become the typed `ServerMessage`s every
    // consumer (WS clients, the in-process Kirigami shell) sees.
    {
        let profiles_for_events = profiles_mgr.clone();
        let mut profile_rx = profiles_mgr.subscribe();
        let bc_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            use snitchwatch_bridge::profiles::ProfileEvent as Evt;
            use snitchwatch_bridge::translator::downstream::{
                build_profile_changed, build_set_profiles,
            };
            while let Ok(evt) = profile_rx.recv().await {
                match evt {
                    Evt::ProfilesChanged => {
                        if let Ok(m) = build_set_profiles(&profiles_for_events).await {
                            let _ = bc_tx.send(m);
                        }
                    }
                    Evt::ActiveProfileChanged { profile_id } => {
                        let _ = bc_tx.send(build_profile_changed(profile_id));
                        if let Ok(m) = build_set_profiles(&profiles_for_events).await {
                            let _ = bc_tx.send(m);
                        }
                    }
                }
            }
        });
    }

    // --- Upstream pump: WS client messages → cache (→ oneshot resolve) -------
    // Profile-related messages are routed to `ProfilesManager` directly
    // (mirroring `handle_blocklist_action`'s treatment of blocklist
    // messages); everything else goes through the connection-cache pump.
    let cache_for_upstream = cache.clone();
    let profiles_for_upstream = profiles_mgr;
    let blocklists_for_upstream = blocklists_mgr;
    let snapshot_tx = broadcast_tx.clone();
    let tray_pub_for_snapshot = tray_pub.clone();
    let filter_pause_for_pump = filter_pause.clone();
    let presence_for_pump = client_presence.clone();
    let diagnostics_ctx_for_pump = diagnostics_ctx.clone();
    let commands_for_pump = daemon_commands;
    let rules_for_pump = rules.clone();
    let blocklist_worker = blocklist_tasks.worker.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            // Blocklist messages go to the single blocklist worker; queueing
            // never waits on a fetch (issue #45).
            let Some(msg) = blocklist_worker.try_route(msg) else {
                continue;
            };
            let Some(msg) = rules_import.try_route(msg) else {
                continue;
            };
            let Some(msg) = rule_commands.try_route(msg) else {
                continue;
            };
            // Special-cased before is_profile_message/upstream::apply — this
            // changes the shared filter pause + tray state, not cache state
            // those own. See docs/superpowers/plans/2026-07-12-tray-filter-off.md.
            if let ClientMessage::SetFilteringPaused {
                paused,
                duration_secs,
                sender_generation,
                sender_uid,
            } = msg
            {
                // Every pause goes through `apply_pause_request` (#47): it is
                // timed, and it applies only while its sender's GUI session
                // generation is current.
                snitchwatch_bridge::client_presence::apply_pause_request(
                    &presence_for_pump,
                    &filter_pause_for_pump,
                    PauseRequest::from_wire(paused, duration_secs),
                    sender_generation,
                    sender_uid,
                );
                // A pause also lets the prompts already waiting through,
                // Allow once (issue #78). A no-op unless a pause applies.
                snitchwatch_bridge::pause_answers::answer_waiting(
                    &filter_pause_for_pump,
                    &cache_for_upstream,
                    &snapshot_tx,
                )
                .await;
                // Always, even for an ignored or rejected request, so every
                // GUI and the tray show the state that is actually in effect.
                announce_pause_state(&filter_pause_for_pump, &cache_for_upstream, &snapshot_tx)
                    .await;
                continue;
            }
            if let ClientMessage::DecideLater { row_id } = &msg {
                // Needs the daemon's settings, which `upstream::apply` doesn't
                // have (prompt-slot plan Part C).
                if let Err(e) = snitchwatch_bridge::deferred_answers::decide_later(
                    &cache_for_upstream,
                    &daemon_config_for_pump,
                    &snapshot_tx,
                    row_id,
                )
                .await
                {
                    warn!(error = %e, "decide later not applied");
                }
                continue;
            }
            if let ClientMessage::RecheckDiagnostics = msg {
                // The user-driven "re-baseline": clear stored daemon alerts
                // before re-running the report, rather than on every
                // subscribe() — see `daemon_alerts`'s module doc for why a
                // fresh subscribe is the wrong trigger. A problem that
                // persists will re-alert on the daemon's next restart.
                diagnostics_ctx_for_pump.clear_alerts();
                let _ = snapshot_tx.send(ServerMessage::DiagnosticsReport {
                    checks: diagnostics_ctx_for_pump.report(),
                });
                continue;
            }
            if is_profile_message(&msg) {
                profile_actions::handle(profiles_for_upstream.clone(), msg, &snapshot_tx).await;
                continue;
            }
            let effect = {
                let mut cache = cache_for_upstream.lock().await;
                upstream::apply(&mut cache, msg)
            };
            match effect {
                Ok(UpstreamEffect::SnapshotRequested) => {
                    // A feed consumer lagged past delta messages and asked for
                    // full state. Re-broadcast the snapshots the bridge itself
                    // owns: connection rows (clear + full insert, the same
                    // sequence a fresh view needs), blocklists, profiles,
                    // the daemon's rules once a snapshot has been committed
                    // (see `ClientMessage::RequestSnapshot` docs), diagnostics,
                    // tray and filter-pause state.
                    let rows = cache_for_upstream.lock().await.rows().to_vec();
                    let _ = snapshot_tx.send(ServerMessage::ClearConnectionRows);
                    if !rows.is_empty() {
                        let _ = snapshot_tx.send(ServerMessage::InsertConnectionRows { rows });
                    }
                    match downstream::build_set_blocklists(&blocklists_for_upstream).await {
                        Ok(m) => {
                            let _ = snapshot_tx.send(m);
                        }
                        Err(e) => warn!(error = %e, "snapshot: blocklists rebuild failed"),
                    }
                    match downstream::build_set_profiles(&profiles_for_upstream).await {
                        Ok(m) => {
                            let _ = snapshot_tx.send(m);
                        }
                        Err(e) => warn!(error = %e, "snapshot: profiles rebuild failed"),
                    }
                    publish_rules(&rules_for_pump, &snapshot_tx);
                    let _ = snapshot_tx.send(ServerMessage::DiagnosticsReport {
                        checks: diagnostics_ctx_for_pump.report(),
                    });
                    let _ = snapshot_tx.send(ServerMessage::TrayState {
                        state: tray_pub_for_snapshot.subscribe().borrow().clone(),
                    });
                    prompt_slot_for_pump.announce(&snapshot_tx);
                    rule_hits_for_pump.announce(&snapshot_tx);
                    // Including `paused: false`: a GUI that was away when a
                    // pause ended learns it here. Sent under the cache lock,
                    // like every other pause announcement, so it can't
                    // overtake a newer one.
                    {
                        let _cache = cache_for_upstream.lock().await;
                        let _ = snapshot_tx.send(filter_pause_for_pump.state().to_message());
                    }
                    info!("re-broadcast state snapshots after feed lag");
                }
                Ok(UpstreamEffect::VerdictApplied { row_id, .. }) => {
                    // `ConnectionCache::resolve` updates its authoritative row
                    // and wakes the blocked AskRule RPC, but the cache itself
                    // deliberately has no broadcast dependency. Fan the updated
                    // row back out here so every live UI replaces its pending
                    // row immediately after an Allow/Deny click.
                    let updated_row = cache_for_upstream
                        .lock()
                        .await
                        .rows()
                        .iter()
                        .find(|row| row.id == row_id)
                        .cloned();
                    if let Some(row) = updated_row {
                        if let Err(e) = snapshot_tx
                            .send(ServerMessage::UpdateConnectionRows { rows: vec![row] })
                        {
                            warn!(error = %e, "verdict update broadcast failed");
                        }
                    } else {
                        error!(%row_id, "verdict applied but resolved row is absent from cache");
                    }
                    info!(%row_id, "applied verdict and broadcast row update");
                }
                Ok(effect) => {
                    // Rule enable/disable/delete: translate to a daemon
                    // notification and send it down the outbound Notifications
                    // stream(s); `DaemonCommands::send` assigns the id. The
                    // rules cache follows the daemon's OK in reply order; a
                    // spawned waiter re-broadcasts the unchanged list on any
                    // other outcome, so the pump never blocks (#48). Anything
                    // that isn't a rule edit yields `None` and falls through
                    // to the original log line.
                    match notification_for_effect(&effect, 0) {
                        Ok(Some(notification)) => {
                            let action = notification.r#type;
                            match commands_for_pump.send(notification) {
                                Ok(pending) => {
                                    info!(id = pending.id(), action, "sent rule command to daemon");
                                    tokio::spawn(settle_rule_command(
                                        pending,
                                        rules_for_pump.clone(),
                                        snapshot_tx.clone(),
                                        RULE_COMMAND_TIMEOUT,
                                    ));
                                }
                                // No daemon stream took it. Dropping is
                                // correct — the daemon reloads its own rules on
                                // connect, so there is nothing to replay. The
                                // list is re-sent to undo the GUI's optimistic
                                // change.
                                Err(e) => {
                                    warn!(action, error = %e, "rule command dropped");
                                    publish_rules(&rules_for_pump, &snapshot_tx);
                                }
                            }
                        }
                        Ok(None) => info!(?effect, "applied upstream effect"),
                        // A rule the daemon would reject silently (see
                        // `rule_from_wire`). Never send it, and re-send the
                        // list to undo the GUI's optimistic change. The rule
                        // body and name are GUI/daemon-supplied text: log only
                        // the request kind and the name's length.
                        Err(e) => {
                            let (kind, name_len) = match &effect {
                                UpstreamEffect::AddRule { rule } => (
                                    "add",
                                    rule.get("name")
                                        .and_then(|n| n.as_str())
                                        .map_or(0, str::len),
                                ),
                                UpstreamEffect::UpdateRule { rule_id, .. } => {
                                    ("update", rule_id.len())
                                }
                                UpstreamEffect::DeleteRule { rule_id } => ("delete", rule_id.len()),
                                _ => ("other", 0),
                            };
                            error!(error = %e, kind, name_len, "refusing to send malformed rule to daemon");
                            publish_rules(&rules_for_pump, &snapshot_tx);
                        }
                    }
                }
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    // --- Traffic pump: connection-row byte counters → binned TrafficEvents --
    // Additive: subscribes to the same outbound broadcast every other
    // consumer uses and folds each connection-row batch's byte counters
    // through `TrafficTracker` (wrapping the existing, already-tested
    // `TrafficBinner`), re-broadcasting the result as `TrafficEvents` — the
    // one typed traffic variant the native Kirigami shell's `TrafficModel`
    // consumes (`bridge_dispatch::interests_traffic`). Never touches the
    // legacy `SetTrafficData`/`UpdateTrafficData` variants.
    let mut traffic_rx = broadcast_tx.subscribe();
    let traffic_tx = broadcast_tx.clone();
    tokio::spawn(async move {
        let mut tracker = TrafficTracker::new(TRAFFIC_WINDOW_SECONDS);
        loop {
            let msg = match traffic_rx.recv().await {
                Ok(msg) => msg,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "traffic pump lagged behind broadcast");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let rows = match &msg {
                ServerMessage::InsertConnectionRows { rows } => rows,
                ServerMessage::UpdateConnectionRows { rows } => rows,
                _ => continue,
            };
            if rows.is_empty() {
                continue;
            }
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let events = tracker.record_rows(now_ms, rows);
            if traffic_tx.receiver_count() > 0 {
                if let Err(e) = traffic_tx.send(ServerMessage::TrafficEvents { events }) {
                    warn!(error = %e, "traffic pump: broadcast send failed");
                }
            }
        }
    });

    Ok(RunningBridge {
        ws_socket_path: config.ws_socket_path,
        ws_token_path,
        ws_token: token,
        grpc_endpoint,
        broadcast_tx,
        inbound_tx,
        tray_rx,
        notice_rx,
        daemon_stream_ready,
        #[cfg(test)]
        client_presence,
        ws_shutdown_tx: Some(ws_shutdown_tx),
        grpc_shutdown_tx: Some(grpc_shutdown_tx),
        watchdog_handle,
        pause_expiry_handle,
        pause_clear_handle,
        blocklist_tasks,
        profile_tasks,
        rule_hits,
        rule_hits_ticker,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use mock_opensnitchd::MockOpensnitchd;
    use snitchwatch_bridge::ws_messages::{VerdictAction, VerdictDuration, VerdictScope};
    use snitchwatch_proto::protocol::Connection;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn system_permissions_peer_helper() {
        let Some(dir) = std::env::var_os("SNITCHWATCH_TEST_PERMISSION_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let token_path = dir.join("auth/token");
        let role = std::env::var("SNITCHWATCH_TEST_PERMISSION_ROLE").unwrap();
        if role == "service" || role == "service-mismatch" {
            assert_eq!(unsafe { libc::geteuid() }, 65531);
            assert_eq!(unsafe { libc::getegid() }, 65531);
            if role == "service-mismatch" {
                let original = auth::read_token_file(&token_path).unwrap();
                for _ in 0..2 {
                    let error =
                        auth::write_system_token_file(&Token::generate(), &token_path).unwrap_err();
                    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                    assert!(error
                        .to_string()
                        .contains("system token group did not inherit auth directory group"));
                    assert!(original.matches(auth::read_token_file(&token_path).unwrap().as_str()));
                    assert!(!dir
                        .join(format!("auth/.token.{}.tmp", std::process::id()))
                        .exists());
                }
                return;
            }
            auth::write_system_token_file(&Token::generate(), &token_path).unwrap();
            let metadata = std::fs::metadata(&token_path).unwrap();
            assert_eq!(
                (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
                (65531, 65533, 0o640)
            );
            return;
        }
        let member = role == "member";
        let gui = std::os::unix::net::UnixStream::connect(dir.join("bridge.sock"));
        let token = auth::read_token_file(&token_path);
        if member {
            assert!(gui.is_ok(), "UI-group member must be able to connect");
            assert_eq!(token.unwrap().as_str().len(), 64);
        } else {
            assert_eq!(
                gui.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert_eq!(
                token.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
        let daemon = std::os::unix::net::UnixStream::connect(dir.join("opensnitchd.sock"));
        assert_eq!(
            daemon.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        for path in [
            dir.join("bridge.sock"),
            dir.join("opensnitchd.sock"),
            token_path,
        ] {
            assert_eq!(
                std::fs::remove_file(path).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
    }

    #[tokio::test]
    async fn service_token_and_socket_permissions_enforce_the_ui_group_across_identities() {
        use std::os::unix::process::CommandExt;
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("run this test as root to verify distinct service/UI identities");
            return;
        }
        // Match /run's native tmpfs semantics. Some rootless development
        // overlays report setgid directories but do not inherit their group.
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
        let gui_path = dir.path().join("bridge.sock");
        let daemon_path = dir.path().join("opensnitchd.sock");
        let _gui = UnixListener::bind(&gui_path).unwrap();
        let _daemon = UnixListener::bind(&daemon_path).unwrap();
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir(&auth_dir).unwrap();
        for (path, uid, gid, mode) in [
            (&gui_path, 0, 65533, 0o660),
            (&daemon_path, 0, 0, 0o600),
            (&auth_dir, 65531, 65533, 0o2750),
        ] {
            use std::os::unix::ffi::OsStrExt;
            let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::chown(path_c.as_ptr(), uid, gid) }, 0);
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(
            std::fs::metadata(&auth_dir).unwrap().mode() & 0o7777,
            0o2750
        );
        for (role, uid, gid) in [
            ("service", 65531, 65531),
            ("member", 65534, 65533),
            ("nonmember", 65532, 65532),
            ("service-mismatch", 65531, 65531),
        ] {
            if role == "service-mismatch" {
                std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o750))
                    .unwrap();
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "tests::system_permissions_peer_helper",
                    "--nocapture",
                ])
                .env("SNITCHWATCH_TEST_PERMISSION_DIR", dir.path())
                .env("SNITCHWATCH_TEST_PERMISSION_ROLE", role);
            // Only async-signal-safe credential syscalls in the forked child.
            // Clear inherited groups before dropping root, so no membership
            // from the container runner can invalidate the negative checks.
            unsafe {
                child.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(gid) != 0
                        || libc::setuid(uid) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            assert!(
                child.status().unwrap().success(),
                "{role} permission checks failed"
            );
            if role == "service-mismatch" {
                std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o2750))
                    .unwrap();
            }
        }
        let token = std::fs::metadata(auth_dir.join("token")).unwrap();
        assert_eq!(
            (token.uid(), token.gid(), token.mode() & 0o777),
            (65531, 65533, 0o640)
        );
        assert_eq!(
            std::fs::metadata(&auth_dir).unwrap().mode() & 0o7777,
            0o2750
        );
        assert_eq!(std::fs::metadata(&gui_path).unwrap().mode() & 0o777, 0o660);
        assert_eq!(
            std::fs::metadata(&daemon_path).unwrap().mode() & 0o777,
            0o600
        );
    }

    // This helper runs in a fresh process so credentials can be changed safely,
    // without mutating the credentials of a running multithreaded test suite.
    #[test]
    fn non_root_daemon_peer_helper() {
        use std::io::Read;
        let Some(path) = std::env::var_os("SNITCHWATCH_TEST_PEER_SOCKET") else {
            return;
        };
        assert_ne!(unsafe { libc::geteuid() }, 0);
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert_eq!(
            stream.read(&mut [0u8; 1]).unwrap(),
            0,
            "non-root peer must be disconnected"
        );
    }

    #[tokio::test]
    async fn root_unix_incoming_rejects_non_root_and_keeps_accepting_every_peer() {
        use std::os::unix::process::CommandExt;
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("run this test as root to verify both credential classes");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = dir.path().join("grpc.sock");
        let mut incoming = RootUnixIncoming(UnixListener::bind(&path).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        for _ in 0..2 {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::non_root_daemon_peer_helper",
                    "--nocapture",
                ])
                .env("SNITCHWATCH_TEST_PEER_SOCKET", &path)
                .uid(65534)
                .gid(65534)
                .spawn()
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), incoming.next())
                    .await
                    .is_err()
            );
            assert!(child.wait().unwrap().success());
            let _root = UnixStream::connect(&path).await.unwrap();
            let accepted = tokio::time::timeout(Duration::from_secs(2), incoming.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(accepted.peer_cred().unwrap().uid(), 0);
        }
    }

    #[tokio::test]
    async fn activated_unix_ask_rule_roundtrip_preserves_socket_ownership_and_modes() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("run this test as root to exercise the authorized daemon peer");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let gui_path = dir.path().join("bridge.sock");
        let grpc_path = dir.path().join("opensnitchd.sock");
        let gui_listener = UnixListener::bind(&gui_path).unwrap();
        let grpc_listener = UnixListener::bind(&grpc_path).unwrap();
        std::fs::set_permissions(&gui_path, std::fs::Permissions::from_mode(0o660)).unwrap();
        std::fs::set_permissions(&grpc_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir(&auth_dir).unwrap();
        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o2750)).unwrap();
        let before_gui = std::fs::metadata(&gui_path).unwrap();
        let before_grpc = std::fs::metadata(&grpc_path).unwrap();
        let before_dir = std::fs::metadata(dir.path()).unwrap();
        let token_path = auth_dir.join("token");
        let config = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: gui_path.clone(),
            cache_capacity: 64,
        };
        let bridge = run_with_incoming(
            config,
            GrpcEndpoint::Unix(grpc_path.clone()),
            RootUnixIncoming(grpc_listener),
            Some(gui_listener),
            Some(token_path.clone()),
            RunOptions::in_process(),
            ANSWER_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(bridge.grpc_endpoint, GrpcEndpoint::Unix(grpc_path.clone()));
        assert!(bridge.grpc_endpoint.tcp_addr().is_none());
        assert_eq!(
            std::fs::metadata(&token_path).unwrap().mode() & 0o777,
            0o640
        );
        assert_eq!(
            std::fs::metadata(&token_path).unwrap().gid(),
            std::fs::metadata(&auth_dir).unwrap().gid()
        );
        assert_eq!(
            std::fs::metadata(&auth_dir).unwrap().mode() & 0o7777,
            0o2750
        );

        let stream = UnixStream::connect(&gui_path).await.unwrap();
        let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
            .await
            .unwrap();
        let token = auth::read_token_file(&token_path).unwrap();
        gui.send(Message::Text(token.as_str().to_owned()))
            .await
            .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(2), gui.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(ack, Message::Text(ref text) if matches!(serde_json::from_str::<ServerMessage>(text), Ok(ServerMessage::Authenticated { .. })))
        );

        let channel = tonic::transport::Endpoint::from_static("http://localhost")
            .connect_with_connector(tower::service_fn(move |_| {
                let path = grpc_path.clone();
                async move {
                    UnixStream::connect(path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .unwrap();
        let ask = tokio::spawn(async move {
            snitchwatch_proto::protocol::ui_client::UiClient::new(channel)
                .ask_rule(Connection {
                    protocol: "tcp".into(),
                    dst_host: "example.com".into(),
                    dst_ip: "93.184.216.34".into(),
                    dst_port: 443,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                })
                .await
                .unwrap()
                .into_inner()
        });
        let pending_id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Message::Text(text) = gui.next().await.unwrap().unwrap() {
                    if let Ok(ServerMessage::InsertConnectionRows { rows }) =
                        serde_json::from_str(&text)
                    {
                        if let Some(row) = rows.into_iter().find(|row| row.action.is_none()) {
                            break row.id;
                        }
                    }
                }
            }
        })
        .await
        .unwrap();
        let verdict = ClientMessage::SetVerdict {
            row_id: pending_id,
            verdict: VerdictAction::Allow,
            scope: VerdictScope::ThisHost,
            duration: Some(VerdictDuration::Once),
            remember: None,
        };
        gui.send(Message::Text(serde_json::to_string(&verdict).unwrap()))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), ask)
                .await
                .unwrap()
                .unwrap()
                .action,
            "allow"
        );
        bridge.shutdown();
        tokio::task::yield_now().await;
        for (path, before) in [
            (&gui_path, before_gui),
            (&dir.path().join("opensnitchd.sock"), before_grpc),
        ] {
            let after = std::fs::metadata(path).unwrap();
            assert_eq!(
                (after.ino(), after.mode(), after.uid(), after.gid()),
                (before.ino(), before.mode(), before.uid(), before.gid())
            );
        }
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().mode(),
            before_dir.mode()
        );
    }

    #[tokio::test]
    async fn run_binds_socket_and_grpc_port_and_shutdown_works() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        assert!(bridge.ws_socket_path.exists());
        assert!(bridge.ws_token_path.exists());
        assert!(bridge.grpc_endpoint.tcp_addr().unwrap().port() != 0);
        bridge.shutdown();
    }

    #[tokio::test]
    async fn exposes_in_process_broadcast_and_inbound_handles() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");

        // Outbound: a subscriber gets the exact ServerMessage the bridge fans out.
        let mut rx = bridge.broadcast_tx.subscribe();
        let msg = ServerMessage::ClearConnectionRows;
        bridge.broadcast_tx.send(msg.clone()).unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("no broadcast within timeout")
            .expect("broadcast channel closed");
        assert_eq!(got, msg);

        // Inbound: a UI-origin ClientMessage is accepted onto the upstream pump.
        bridge
            .inbound_tx
            .send(ClientMessage::Undo)
            .await
            .expect("inbound channel closed");

        bridge.shutdown();
    }

    #[tokio::test]
    async fn verdict_broadcasts_an_updated_non_pending_row() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        let mut rx = bridge.broadcast_tx.subscribe();
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let transport = tokio::net::UnixStream::connect(&bridge.ws_socket_path)
            .await
            .unwrap();
        let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", transport)
            .await
            .unwrap();
        gui.send(Message::Text(bridge.ws_token.as_str().into()))
            .await
            .unwrap();
        let ack = gui.next().await.unwrap().unwrap();
        assert!(matches!(ack, Message::Text(ref text) if text.contains("authenticated")));

        let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
        let ask = tokio::spawn(async move {
            let mut daemon = MockOpensnitchd::connect(grpc_addr).await.unwrap();
            daemon
                .ask_rule(Connection {
                    protocol: "tcp".into(),
                    dst_host: "example.com".into(),
                    dst_ip: "93.184.216.34".into(),
                    dst_port: 443,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                })
                .await
                .unwrap()
        });

        let pending_id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let ServerMessage::InsertConnectionRows { rows } =
                    rx.recv().await.expect("broadcast channel closed")
                {
                    if let Some(row) = rows.into_iter().find(|row| row.action.is_none()) {
                        break row.id;
                    }
                }
            }
        })
        .await
        .expect("pending AskRule row was not broadcast");

        bridge
            .inbound_tx
            .send(ClientMessage::SetVerdict {
                row_id: pending_id.clone(),
                verdict: VerdictAction::Allow,
                scope: VerdictScope::ThisHost,
                duration: Some(VerdictDuration::Once),
                remember: None,
            })
            .await
            .expect("inbound channel closed");

        let updated = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let ServerMessage::UpdateConnectionRows { rows } =
                    rx.recv().await.expect("broadcast channel closed")
                {
                    if let Some(row) = rows.into_iter().find(|row| row.id == pending_id) {
                        break row;
                    }
                }
            }
        })
        .await
        .expect("verdict did not broadcast a row update");
        assert_eq!(updated.action.as_deref(), Some("allow"));

        let rule = ask.await.expect("AskRule task panicked");
        assert_eq!(rule.action, "allow");
        bridge.shutdown();
    }

    #[tokio::test]
    async fn request_snapshot_rebroadcasts_bridge_owned_state() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        let mut rx = bridge.broadcast_tx.subscribe();

        bridge
            .inbound_tx
            .send(ClientMessage::RequestSnapshot)
            .await
            .expect("inbound channel closed");

        // Expected snapshot sequence for an empty bridge: a connections clear
        // (no insert — the cache is empty), then blocklists, profiles, and
        // the current tray value. The latter lets a GUI that subscribed after
        // a state transition render the service-owned shell state correctly.
        // Ignore unrelated interleavings (e.g. traffic pump output) but bound
        // the wait so a missing snapshot fails rather than hangs.
        let mut saw_clear = false;
        let mut saw_blocklists = false;
        let mut saw_profiles = false;
        let mut saw_tray = false;
        let mut saw_pause = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while !(saw_clear && saw_blocklists && saw_profiles && saw_tray && saw_pause) {
            let msg = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("snapshot messages not re-broadcast within timeout")
                .expect("broadcast channel closed");
            match msg {
                ServerMessage::ClearConnectionRows => saw_clear = true,
                ServerMessage::SetBlocklists { .. } => saw_blocklists = true,
                ServerMessage::SetProfiles { .. } => saw_profiles = true,
                ServerMessage::TrayState {
                    state: TrayState::Idle,
                } => saw_tray = true,
                // A GUI that was away when a pause ended learns it here.
                ServerMessage::FilterPauseState {
                    paused: false,
                    expires_at_unix_ms: None,
                } => saw_pause = true,
                _ => {}
            }
        }
        bridge.shutdown();
    }

    #[tokio::test]
    async fn synthetic_connection_activity_is_rebroadcast_as_traffic_events() {
        use snitchwatch_bridge::ws_messages::ConnectionRow;

        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        let mut rx = bridge.broadcast_tx.subscribe();

        // Simulate what `UiService::ask_rule` broadcasts on a real connection
        // (a synthetic row with non-zero byte counters, since production
        // `ask_rule` rows start at zero — this exercises the pump's mapping
        // end-to-end regardless of what today's actual producer sends).
        let row = ConnectionRow {
            id: "ask-1".into(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 1234,
            bytes_received: 5678,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
        };
        bridge
            .broadcast_tx
            .send(ServerMessage::InsertConnectionRows {
                rows: vec![row.clone()],
            })
            .expect("broadcast send failed");

        // First: the original InsertConnectionRows, echoed to every subscriber
        // (including this test's own, exactly like a browser WS client).
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("no broadcast within timeout")
            .expect("broadcast channel closed");
        assert_eq!(
            first,
            ServerMessage::InsertConnectionRows { rows: vec![row] }
        );

        // Second: the traffic pump's derived TrafficEvents, mapping
        // bytes_sent -> bytesOut and bytes_received -> bytesIn.
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("no TrafficEvents broadcast within timeout")
            .expect("broadcast channel closed");
        match second {
            ServerMessage::TrafficEvents { events } => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].bytes_in, 5678);
                assert_eq!(events[0].bytes_out, 1234);
            }
            other => panic!("expected TrafficEvents, got {other:?}"),
        }

        bridge.shutdown();
    }

    #[tokio::test]
    async fn pause_request_without_an_authenticated_gui_is_ignored() {
        // A pause still queued when its GUI disconnected arrives with no
        // session; it must not re-arm the pause for the next GUI (#47).
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let mut bridge = run(cfg).await.expect("run failed");

        bridge
            .inbound_tx
            .send(set_filtering_paused(true, Some(1800)))
            .await
            .expect("inbound channel closed");
        // The pump always publishes a tray state for a pause request, so
        // wait for it: an applied pause would show FilterOff.
        tokio::time::timeout(Duration::from_secs(5), bridge.tray_rx.changed())
            .await
            .expect("pump did not handle the pause request")
            .unwrap();
        assert_ne!(*bridge.tray_rx.borrow(), TrayState::FilterOff);
        // A GUI arriving afterwards must not inherit a pause. (A GUI that
        // registers before the pump runs is covered by the sender-generation
        // stamp; see `client_presence`'s tests.)
        let _gui = bridge.client_presence.authenticated_session();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), bridge.tray_rx.changed())
                .await
                .is_err(),
            "nothing should re-publish FilterOff"
        );
        assert_ne!(*bridge.tray_rx.borrow(), TrayState::FilterOff);

        bridge.shutdown();
    }

    fn set_filtering_paused(paused: bool, duration_secs: Option<u64>) -> ClientMessage {
        ClientMessage::SetFilteringPaused {
            paused,
            duration_secs,
            sender_generation: None,
            sender_uid: None,
        }
    }

    fn unix_ms_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    async fn test_bridge(dir: &tempfile::TempDir) -> RunningBridge {
        run(BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        })
        .await
        .expect("run failed")
    }

    /// Wait for the next `FilterPauseState` broadcast, skipping unrelated
    /// messages and counting `FilterPauseExpired` notices on the way.
    async fn next_pause_state(
        rx: &mut broadcast::Receiver<ServerMessage>,
        expiry_notices: &mut usize,
    ) -> (bool, Option<u64>) {
        loop {
            match rx.recv().await.expect("broadcast channel closed") {
                ServerMessage::FilterPauseState {
                    paused,
                    expires_at_unix_ms,
                } => return (paused, expires_at_unix_ms),
                ServerMessage::Notice {
                    notice: Notice::FilterPauseExpired,
                } => *expiry_notices += 1,
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn set_filtering_paused_toggles_tray_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = test_bridge(&dir).await;
        let mut rx = bridge.broadcast_tx.subscribe();
        let mut expiry_notices = 0;
        // A pause only takes effect while a GUI is authenticated (#47).
        let _gui = bridge.client_presence.authenticated_session();

        let before = unix_ms_now();
        bridge
            .inbound_tx
            .send(set_filtering_paused(true, Some(1800)))
            .await
            .expect("inbound channel closed");
        bridge.tray_rx.changed().await.unwrap();
        assert_eq!(*bridge.tray_rx.borrow(), TrayState::FilterOff);
        let (paused, expires_at) = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("pause state was not broadcast");
        assert!(paused);
        let expires_at = expires_at.expect("a pause carries its end time");
        assert!(
            (before + 1_800_000..=unix_ms_now() + 1_800_000).contains(&expires_at),
            "a 30-minute pause must end 30 minutes from now, got {expires_at}"
        );

        bridge
            .inbound_tx
            .send(set_filtering_paused(false, None))
            .await
            .expect("inbound channel closed");
        bridge.tray_rx.changed().await.unwrap();
        assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);
        let resumed = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("resume was not broadcast");
        assert_eq!(resumed, (false, None));
        assert_eq!(expiry_notices, 0);

        bridge.shutdown();
    }

    #[tokio::test]
    async fn a_snapshot_mid_pause_reports_the_pause_and_its_end() {
        // A GUI that connects mid-pause learns the state and the end time.
        let dir = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&dir).await;
        let mut rx = bridge.broadcast_tx.subscribe();
        let mut expiry_notices = 0;
        let _gui = bridge.client_presence.authenticated_session();
        bridge
            .inbound_tx
            .send(set_filtering_paused(true, Some(1800)))
            .await
            .expect("inbound channel closed");
        let (_, announced_end) = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("pause state was not broadcast");
        let announced_end = announced_end.expect("a pause carries its end time");

        bridge
            .inbound_tx
            .send(ClientMessage::RequestSnapshot)
            .await
            .expect("inbound channel closed");
        let (paused, snapshot_end) = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("the snapshot carried no pause state");
        assert!(paused);
        let snapshot_end = snapshot_end.expect("a pause carries its end time");
        assert!(
            snapshot_end.abs_diff(announced_end) < 1_000,
            "snapshot end {snapshot_end} drifted from {announced_end}"
        );
        assert!(
            snapshot_end > unix_ms_now() + 1_790_000,
            "most of the 30 minutes remain"
        );
        bridge.shutdown();
    }

    #[tokio::test]
    async fn legacy_pause_without_a_duration_expires_after_five_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = test_bridge(&dir).await;
        let mut rx = bridge.broadcast_tx.subscribe();
        let mut expiry_notices = 0;
        let _gui = bridge.client_presence.authenticated_session();

        // The bridge started on real time; from here on the test drives the
        // clock (auto-advancing whenever the runtime is idle).
        tokio::time::pause();
        let start = tokio::time::Instant::now();
        bridge
            .inbound_tx
            .send(set_filtering_paused(true, None))
            .await
            .expect("inbound channel closed");
        let (paused, _) = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("pause state was not broadcast");
        assert!(paused, "an old client's pause must still work");
        assert_eq!(*bridge.tray_rx.borrow_and_update(), TrayState::FilterOff);

        let (paused, expires_at) = tokio::time::timeout(
            Duration::from_secs(600),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("the pause never expired");
        let elapsed = start.elapsed();
        assert_eq!((paused, expires_at), (false, None));
        assert!(
            (Duration::from_secs(300)..=Duration::from_secs(301)).contains(&elapsed),
            "legacy pause ended after {elapsed:?}, not 300 s"
        );
        assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);

        // The expiry notice is relayed separately; collect it, and make sure
        // there is exactly one.
        tokio::time::sleep(Duration::from_secs(10)).await;
        while let Ok(msg) = rx.try_recv() {
            if let ServerMessage::Notice {
                notice: Notice::FilterPauseExpired,
            } = msg
            {
                expiry_notices += 1;
            }
        }
        assert_eq!(expiry_notices, 1);
        bridge.shutdown();
    }

    #[tokio::test]
    async fn losing_the_last_gui_clears_the_pause_without_an_expiry_notice() {
        let dir = tempfile::tempdir().unwrap();
        let mut bridge = test_bridge(&dir).await;
        let mut rx = bridge.broadcast_tx.subscribe();
        let mut expiry_notices = 0;
        let gui = bridge.client_presence.authenticated_session();

        bridge
            .inbound_tx
            .send(set_filtering_paused(true, Some(3600)))
            .await
            .expect("inbound channel closed");
        let (paused, _) = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("pause state was not broadcast");
        assert!(paused);

        drop(gui);
        let cleared = tokio::time::timeout(
            Duration::from_secs(5),
            next_pause_state(&mut rx, &mut expiry_notices),
        )
        .await
        .expect("clearing the pause was not broadcast");
        assert_eq!(cleared, (false, None));
        assert_eq!(*bridge.tray_rx.borrow_and_update(), TrayState::Idle);

        tokio::time::sleep(Duration::from_millis(200)).await;
        while let Ok(msg) = rx.try_recv() {
            if let ServerMessage::Notice {
                notice: Notice::FilterPauseExpired,
            } = msg
            {
                expiry_notices += 1;
            }
        }
        assert_eq!(expiry_notices, 0, "no GUI is left to show an expiry");
        bridge.shutdown();
    }
}
