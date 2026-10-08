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
mod relays;
mod replier;
mod rule_commands;
mod rule_effect;
pub mod rule_hits_storage;
mod rules_import;
pub mod storage;
#[cfg(test)]
mod test_daemon;

use activation::RootUnixIncoming;
use profile_storage::is_profile_message;

pub use storage::{
    resolve_storage, BridgeMode, EphemeralReason, RunOptions, Storage, PER_USER_REASON,
};

use anyhow::{Context, Result};
use snitchwatch_bridge::auth::{self, Token};
use snitchwatch_bridge::blocklists::worker::{BlocklistTasks, DEFAULT_REFRESH_TICK};
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::cache::rule_hits_handle::RuleHitsHandle;
use snitchwatch_bridge::cache::rules::{prune_expired_rules_every, publish_rules};
use snitchwatch_bridge::daemon_commands::DaemonTransport;
use snitchwatch_bridge::deferred_answers::ANSWER_TIMEOUT;
use snitchwatch_bridge::filter_pause::{FilterPause, PauseRequest};
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::notice::{Notice, NoticeBus};
use snitchwatch_bridge::profiles::network_watcher;
use snitchwatch_bridge::translator::downstream;
use snitchwatch_bridge::translator::upstream::{self, UpstreamEffect};
use snitchwatch_bridge::tray_state::{TrayState, TrayStatePublisher};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};
use tonic::transport::Server;
use tracing::{error, info, warn};

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

    // Subscribe before spawning the relays (see `relays::spawn_tray_and_notice_relays`).
    relays::spawn_tray_and_notice_relays(&tray_pub, &notice_bus, &broadcast_tx);
    let (pause_clear_handle, pause_expiry_handle) = relays::spawn_pause_tasks(
        &client_presence,
        &filter_pause,
        &cache,
        &broadcast_tx,
        &notice_bus,
    );

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

    relays::spawn_profile_event_relay(&profiles_mgr, &broadcast_tx);

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
                relays::announce_pause_state(
                    &filter_pause_for_pump,
                    &cache_for_upstream,
                    &snapshot_tx,
                )
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
                            let _ = snapshot_tx.send(downstream::build_set_blocklist_leftovers(
                                &blocklists_for_upstream,
                            ));
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
                    rule_effect::send(&effect, &commands_for_pump, &rules_for_pump, &snapshot_tx);
                }
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    relays::spawn_traffic_pump(&broadcast_tx);

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
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
mod pause_tests;
