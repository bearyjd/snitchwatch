//! Handle to a running bridge.

use crate::config::GrpcEndpoint;
use snitchwatch_bridge::auth::Token;
use snitchwatch_bridge::blocklists::worker::BlocklistTasks;
use snitchwatch_bridge::cache::rule_hits_handle::RuleHitsHandle;
use snitchwatch_bridge::curated::manager::CuratedDefaults;
use snitchwatch_bridge::notice::Notice;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use std::path::PathBuf;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

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
    pub(crate) daemon_stream_ready: watch::Receiver<u64>,
    /// Authenticated GUI sessions. The WS server registers each client after
    /// its handshake; tests register one directly to exercise GUI-gated paths
    /// such as prompts and pausing. Test-only so no caller can hold a lease
    /// that leaves the bridge permanently "attended".
    #[cfg(test)]
    pub(crate) client_presence: snitchwatch_bridge::client_presence::ClientPresence,
    pub(crate) ws_shutdown_tx: Option<oneshot::Sender<()>>,
    pub(crate) grpc_shutdown_tx: Option<oneshot::Sender<()>>,
    /// The daemon-down watchdog task (`daemon_watchdog::run`). It has no
    /// external state to flush on stop — unlike the WS/gRPC servers, an
    /// abort is sufficient rather than a graceful oneshot handshake.
    pub(crate) watchdog_handle: tokio::task::JoinHandle<()>,
    /// The filter-pause expiry task (`filter_pause::expire_pause_on_deadline`).
    /// Like the watchdog, it never ends on its own.
    pub(crate) pause_expiry_handle: tokio::task::JoinHandle<()>,
    /// The last-loss pause clear (`clear_pause_on_last_session_loss`). It
    /// holds the cache and broadcast sender, so stop it with the bridge.
    pub(crate) pause_clear_handle: tokio::task::JoinHandle<()>,
    /// The blocklist worker, its refresh loop and event pump (issue #45).
    pub(crate) blocklist_tasks: BlocklistTasks,
    pub(crate) analysis_exports: crate::analysis_export::AnalysisExports,
    /// The profile enforcer and auto-switch tasks (issue #46).
    pub(crate) profile_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Per-rule hit counts, saved once more at shutdown, and the ticker that
    /// broadcasts and saves them in between.
    pub(crate) rule_hits: RuleHitsHandle,
    pub(crate) rule_hits_ticker: tokio::task::JoinHandle<()>,
    /// The recommended rules' reconcile loop (prompt-slot D), and their
    /// choices, saved once more at shutdown.
    pub(crate) curated_handle: tokio::task::JoinHandle<()>,
    pub(crate) curated: CuratedDefaults,
}

impl RunningBridge {
    /// Generation bumped each time a daemon `Notifications` stream says HELLO
    /// and becomes the one rule commands are correlated with. Wait with a
    /// level check, `wait_for(|g| *g >= 1)`: `changed()` hangs when the HELLO
    /// was handled before the receiver was taken.
    pub fn daemon_stream_ready(&self) -> watch::Receiver<u64> {
        self.daemon_stream_ready.clone()
    }

    /// Where the recommended rules look up the services' accounts (#120
    /// item 38): none until set, so every account is unknown and nothing
    /// pinned to one is installed. Only [`run_system`] sets the host's
    /// account database (#157); tests set a fake.
    pub fn set_curated_uid_lookup(&self, lookup: snitchwatch_bridge::accounts::UidLookup) {
        self.curated.set_uid_lookup(lookup);
    }

    /// [`Self::set_curated_uid_lookup`] with each uid found looked back up
    /// with `names`: a uid whose own name isn't the account's is
    /// unconfirmed (security review M1).
    pub fn set_curated_account_lookups(
        &self,
        lookup: snitchwatch_bridge::accounts::UidLookup,
        names: snitchwatch_bridge::accounts::AccountLookup,
    ) {
        self.curated.set_account_lookups(lookup, names);
    }

    /// Signal every background task to stop. Safe to call more than once.
    pub fn shutdown(mut self) {
        self.analysis_exports.stop();
        self.watchdog_handle.abort();
        self.pause_expiry_handle.abort();
        self.pause_clear_handle.abort();
        self.blocklist_tasks.abort();
        self.curated_handle.abort();
        self.curated.save_now();
        for task in &self.profile_tasks {
            task.abort();
        }
        // Aborting the ticker doesn't cancel a save it already handed to
        // `spawn_blocking`: `save_at_stop` waits for one running, and makes
        // one that runs later a no-op.
        self.rule_hits_ticker.abort();
        self.rule_hits.save_at_stop();
        if let Some(tx) = self.ws_shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.grpc_shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
