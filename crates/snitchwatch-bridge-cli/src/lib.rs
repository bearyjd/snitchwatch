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
pub mod cli;

use anyhow::{Context, Result};
use snitchwatch_bridge::auth::{self, Token};
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::BlocklistsManager;
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::cache::traffic_tracker::TrafficTracker;
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::notice::{Notice, NoticeBus};
use snitchwatch_bridge::profiles::network_watcher;
use snitchwatch_bridge::profiles::store::ProfileStore;
use snitchwatch_bridge::profiles::ProfilesManager;
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
}

impl RunningBridge {
    /// Signal every background task to stop. Safe to call more than once.
    pub fn shutdown(mut self) {
        self.watchdog_handle.abort();
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
pub async fn run(config: BridgeConfig) -> Result<RunningBridge> {
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
    )
    .await
}

async fn run_with_incoming<I, IO>(
    config: BridgeConfig,
    grpc_endpoint: GrpcEndpoint,
    incoming: I,
    activated_ws: Option<UnixListener>,
    system_token_path: Option<PathBuf>,
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
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        config.cache_capacity,
        tray_pub.clone(),
    )));

    // --- BlocklistsManager (in-memory store; callers may swap in a persisted one) ---
    let blocklists_store = Arc::new(
        BlocklistStore::open_in_memory().context("failed to open in-memory blocklist store")?,
    );
    let blocklists_mgr = Arc::new(BlocklistsManager::new(blocklists_store));

    // --- ProfilesManager (in-memory store; callers may swap in a persisted one) ---
    let profiles_store =
        Arc::new(ProfileStore::open_in_memory().context("failed to open in-memory profile store")?);
    let profiles_mgr = Arc::new(ProfilesManager::new(profiles_store));

    // Network-driven auto-activation. `connect_watcher` degrades to a no-op
    // watcher (manual-activation-only) if NetworkManager/D-Bus isn't
    // reachable — never fails `run`, never panics.
    let network_watcher = network_watcher::connect_watcher().await;
    let _profile_auto_switch_handle = profiles_mgr.clone().spawn_auto_switch(network_watcher);

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

    let client_presence = snitchwatch_bridge::client_presence::ClientPresence::default();
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
    let notice_bus = Arc::new(NoticeBus::new());
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

    // Shared with the inbound pump below (SetFilteringPaused toggles it) and
    // read by UiService::ask_rule on every call. Resets to unpaused on every
    // bridge start, matching every other in-memory bridge state.
    let filtering_paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // ...and is cleared when the last authenticated GUI leaves, so a pause
    // doesn't carry over to the next GUI to connect (issue #47; see
    // `apply_pause_request` for the one remaining race).
    {
        let cache = cache.clone();
        tokio::spawn(
            snitchwatch_bridge::client_presence::clear_pause_on_last_session_loss(
                client_presence.session_losses(),
                filtering_paused.clone(),
                move || {
                    let cache = cache.clone();
                    async move { cache.lock().await.resync_tray_state() }
                },
            ),
        );
    }

    let ui_service_inner = UiService::new(
        cache.clone(),
        broadcast_tx.clone(),
        tray_pub.clone(),
        notice_bus.clone(),
        filtering_paused.clone(),
    )
    .with_client_presence(client_presence.clone());
    // Grabbed before `.into_server()` consumes `ui_service_inner` — the
    // daemon-down watchdog below needs this to watch daemon liveness.
    let liveness = ui_service_inner.liveness_handle();

    // Outbound rule commands (enable/disable/delete) for the connected daemon.
    // Same "grab before into_server()" reason as `liveness` above.
    let notifications_tx = ui_service_inner.notifications_handle();
    // Echoed back by the daemon in its NotificationReply. Starts at 1: id 0 is
    // the daemon's own HELLO reply
    // (`vendor/opensnitch/daemon/ui/notifications.go:377`), so a 0 here would
    // be indistinguishable from it.
    let notification_id = Arc::new(std::sync::atomic::AtomicU64::new(1));

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
    // Mirrors `ws_server::serve_with_blocklists`'s blocklist-event pump: the
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
    let tray_pub_for_pause = tray_pub.clone();
    let tray_pub_for_snapshot = tray_pub.clone();
    let filtering_paused_for_pump = filtering_paused.clone();
    let presence_for_pump = client_presence.clone();
    let diagnostics_ctx_for_pump = diagnostics_ctx.clone();
    let notifications_for_pump = notifications_tx.clone();
    let notification_id_for_pump = notification_id.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            // Special-cased before is_profile_message/upstream::apply — this
            // toggles a shared flag + tray state, not cache state those own.
            // See docs/superpowers/plans/2026-07-12-tray-filter-off.md.
            if let ClientMessage::SetFilteringPaused { paused } = msg {
                // Goes through `apply_pause_request`: a pause queued by a GUI
                // that has since disconnected must not take effect (#47).
                let paused = snitchwatch_bridge::client_presence::apply_pause_request(
                    &presence_for_pump,
                    &filtering_paused_for_pump,
                    paused,
                );
                if paused {
                    tray_pub_for_pause.set(TrayState::FilterOff);
                } else {
                    cache_for_upstream.lock().await.resync_tray_state();
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
                use snitchwatch_bridge::translator::upstream::handle_profile_action;
                if let Err(e) = handle_profile_action(profiles_for_upstream.clone(), msg).await {
                    error!(error = %e, "profile action failed");
                }
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
                    // sequence a fresh view needs), blocklists, and profiles.
                    // Rules are excluded — the bridge holds no rule cache (see
                    // `ClientMessage::RequestSnapshot` docs).
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
                    let _ = snapshot_tx.send(ServerMessage::DiagnosticsReport {
                        checks: diagnostics_ctx_for_pump.report(),
                    });
                    let _ = snapshot_tx.send(ServerMessage::TrayState {
                        state: tray_pub_for_snapshot.subscribe().borrow().clone(),
                    });
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
                    // notification and push it down the outbound Notifications
                    // stream. Anything that isn't a rule edit yields `None` and
                    // falls through to the original log line.
                    let id =
                        notification_id_for_pump.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    match notification_for_effect(&effect, id) {
                        Ok(Some(notification)) => {
                            let action = notification.r#type;
                            match notifications_for_pump.send(notification) {
                                Ok(receivers) => {
                                    info!(id, action, receivers, "sent rule command to daemon")
                                }
                                // No daemon holding the stream open. Dropping is
                                // correct — the daemon reloads its own rules on
                                // connect, so there is nothing to replay.
                                Err(_) => {
                                    warn!(id, action, "no daemon connected; rule command dropped")
                                }
                            }
                        }
                        Ok(None) => info!(?effect, "applied upstream effect"),
                        // A rule the daemon would reject silently (see
                        // `rule_from_wire`). Never send it.
                        Err(e) => {
                            error!(error = %e, ?effect, "refusing to send malformed rule to daemon")
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
        #[cfg(test)]
        client_presence,
        ws_shutdown_tx: Some(ws_shutdown_tx),
        grpc_shutdown_tx: Some(grpc_shutdown_tx),
        watchdog_handle,
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
            matches!(ack, Message::Text(ref text) if matches!(serde_json::from_str::<ServerMessage>(text), Ok(ServerMessage::Authenticated)))
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
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while !(saw_clear && saw_blocklists && saw_profiles && saw_tray) {
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
            .send(ClientMessage::SetFilteringPaused { paused: true })
            .await
            .expect("inbound channel closed");
        // The pump always publishes a tray state for a pause request, so
        // wait for it: an applied pause would show FilterOff.
        tokio::time::timeout(Duration::from_secs(5), bridge.tray_rx.changed())
            .await
            .expect("pump did not handle the pause request")
            .unwrap();
        assert_ne!(*bridge.tray_rx.borrow(), TrayState::FilterOff);
        // A GUI arriving afterwards must not inherit a pause. (Registering
        // before the pump ran would be the documented remaining race.)
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

    #[tokio::test]
    async fn set_filtering_paused_toggles_tray_state() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: dir.path().join("bridge.sock"),
            cache_capacity: 64,
        };
        let mut bridge = run(cfg).await.expect("run failed");
        // A pause only takes effect while a GUI is authenticated (#47).
        let _gui = bridge.client_presence.authenticated_session();

        bridge
            .inbound_tx
            .send(ClientMessage::SetFilteringPaused { paused: true })
            .await
            .expect("inbound channel closed");
        bridge.tray_rx.changed().await.unwrap();
        assert_eq!(*bridge.tray_rx.borrow(), TrayState::FilterOff);

        bridge
            .inbound_tx
            .send(ClientMessage::SetFilteringPaused { paused: false })
            .await
            .expect("inbound channel closed");
        bridge.tray_rx.changed().await.unwrap();
        assert_eq!(*bridge.tray_rx.borrow(), TrayState::Idle);

        bridge.shutdown();
    }
}
