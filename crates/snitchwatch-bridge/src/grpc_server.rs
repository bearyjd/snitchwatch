//! Bridge-side gRPC server: implements `protocol.UI` and is dialed by
//! opensnitchd as the gRPC client.
//!
//! Replaces the M1 dial-out flow that lived in the now-deleted
//! `grpc_client.rs` and `translator/downstream.rs` envelope hack.

use crate::cache::connections::{ConnectionCache, Verdict};
use crate::daemon_alerts::DaemonAlertStore;
use crate::daemon_liveness::StreamGuard;
use crate::diagnostics::DiagnosticsCtx;
use crate::notice::NoticeBus;
use crate::translator::connection::{connection_to_row, event_to_row};
use crate::translator::verdict::verdict_to_rule;
use crate::tray_state::{TrayState, TrayStatePublisher};
use crate::ws_messages::{ServerMessage, VerdictDuration};
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{debug, info, warn};

/// Re-exported so existing call sites (and anything that historically
/// imported it from here) keep working; `daemon_watchdog`/`diagnostics` now
/// import [`crate::daemon_liveness::DaemonLiveness`] directly instead —
/// this gRPC service module shouldn't be a dependency of the watchdog.
pub use crate::daemon_liveness::DaemonLiveness;

/// How long a `RecentBlock` tray state stays up before reverting to
/// whatever `Idle`/`Pending(n)` the cache actually holds. A UX default with
/// no prior precedent in this codebase to match (long enough for a glance
/// at the tray tooltip, short enough not to hide a still-accurate `Pending`
/// count for long) — easy to tune later, not a measured value.
const RECENT_BLOCK_TTL: Duration = Duration::from_secs(5);

/// Bridge-side gRPC server state. Handed to `UiServer::new` for tonic.
#[derive(Clone)]
pub struct UiService {
    cache: Arc<Mutex<ConnectionCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
    next_ask_id: Arc<AtomicU64>,
    tray_pub: Arc<TrayStatePublisher>,
    notice_bus: Arc<NoticeBus>,
    /// Refreshed by every daemon-facing handler; read by
    /// `daemon_watchdog::run` (via [`Self::liveness_handle`]) to detect a
    /// stale/unreachable daemon. See [`DaemonLiveness`]'s doc comment for
    /// why raw ping recency alone isn't enough.
    liveness: DaemonLiveness,
    /// Guards `RecentBlock`'s revert timer against a race between two
    /// blocks in quick succession: each spawned revert only fires if this
    /// counter still matches the value it captured at spawn time, so an
    /// older block's timer never stomps a newer block's still-live display.
    block_generation: Arc<AtomicU64>,
    /// True while the user has paused interactive filtering (tray
    /// "Pause filtering"). Unlike `liveness`/`block_generation`, this must
    /// be *writable* from outside `UiService` (the inbound `ClientMessage`
    /// pump toggles it) as well as readable from inside `ask_rule` — the
    /// same shape `tray_pub`/`cache` already have — so it's a genuine
    /// constructor parameter, not internal-only state. See
    /// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`.
    filtering_paused: Arc<AtomicBool>,
    /// Set on every `subscribe()` call from opensnitchd's `is_firewall_running`
    /// field on its `ClientConfig`; read by a later diagnostics report
    /// assembler (via [`Self::firewall_status_handle`]) alongside the local
    /// kernel checks. `None` until the daemon has subscribed at least once.
    firewall_status: Arc<StdMutex<Option<bool>>>,
    /// Most recent ERROR/WARNING alert per `Alert.What`, recorded by
    /// `post_alert` and overlaid by `DiagnosticsCtx::report()` onto the
    /// existing checks (see `daemon_alerts` module doc for the issue #6
    /// rationale). Deliberately *not* cleared on `subscribe()` — see that
    /// module's doc comment for why; it's cleared explicitly by
    /// `ClientMessage::RecheckDiagnostics` instead (via
    /// `DiagnosticsCtx::clear_alerts`, in `snitchwatch-bridge-cli::run`).
    alert_store: Arc<DaemonAlertStore>,
    /// Late-bound handle to the full diagnostics assembler, so `post_alert`
    /// can push a fresh `DiagnosticsReport` the moment a daemon alert
    /// arrives, rather than waiting for the next poll/recheck.
    ///
    /// This can't be a constructor parameter: `DiagnosticsCtx::new` needs
    /// `firewall_status_handle()`/`alert_store_handle()` from an already-
    /// constructed `UiService`, so building it first isn't possible without
    /// either duplicating that state outside `UiService` or breaking every
    /// existing test call site that only expects the five original
    /// constructor args. `snitchwatch-bridge-cli::run` fills this in via
    /// [`Self::diagnostics_ctx_slot`] once `DiagnosticsCtx` exists; unset
    /// (e.g. in most unit tests here) means `post_alert` still records the
    /// alert but skips the push broadcast.
    diagnostics_ctx: Arc<OnceLock<Arc<DiagnosticsCtx>>>,
}

impl UiService {
    pub fn new(
        cache: Arc<Mutex<ConnectionCache>>,
        broadcast: broadcast::Sender<ServerMessage>,
        tray_pub: Arc<TrayStatePublisher>,
        notice_bus: Arc<NoticeBus>,
        filtering_paused: Arc<AtomicBool>,
    ) -> Self {
        Self {
            cache,
            broadcast,
            next_ask_id: Arc::new(AtomicU64::new(1)),
            tray_pub,
            notice_bus,
            liveness: DaemonLiveness::new(),
            block_generation: Arc::new(AtomicU64::new(0)),
            filtering_paused,
            firewall_status: Arc::new(StdMutex::new(None)),
            alert_store: Arc::new(DaemonAlertStore::new()),
            diagnostics_ctx: Arc::new(OnceLock::new()),
        }
    }

    /// Convenience: wrap into a tonic `UiServer<UiService>` ready for
    /// `Server::builder().add_service(...)`.
    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }

    /// Handle to the daemon-liveness tracker, for `daemon_watchdog::run` and
    /// `DiagnosticsCtx` to poll. Exposed as an accessor (not a `new()`
    /// parameter) so existing call sites don't need to change.
    pub fn liveness_handle(&self) -> DaemonLiveness {
        self.liveness.clone()
    }

    /// Handle to the last-observed firewall status (from opensnitchd's
    /// `subscribe()` handshake), for a later diagnostics report assembler
    /// to poll. Exposed as an accessor for the same reason as
    /// `liveness_handle`.
    pub fn firewall_status_handle(&self) -> Arc<StdMutex<Option<bool>>> {
        self.firewall_status.clone()
    }

    /// Handle to the daemon-alert store, for `DiagnosticsCtx::new` to overlay
    /// onto its checks. Exposed as an accessor for the same reason as
    /// `firewall_status_handle`.
    pub fn alert_store_handle(&self) -> Arc<DaemonAlertStore> {
        self.alert_store.clone()
    }

    /// Late-binds the diagnostics assembler `post_alert` pushes a fresh
    /// report through. Callable exactly once per `UiService`; a second call
    /// is a no-op (mirrors `OnceLock::set`'s own semantics) since only one
    /// `DiagnosticsCtx` is ever constructed per bridge run. See
    /// [`Self::diagnostics_ctx`]'s doc comment for why this is late-bound
    /// rather than a constructor parameter.
    pub fn set_diagnostics_ctx(&self, ctx: Arc<DiagnosticsCtx>) {
        let _ = self.diagnostics_ctx.set(ctx);
    }

    /// Publish `TrayState::RecentBlock` and schedule its own revert after
    /// [`RECENT_BLOCK_TTL`]. If a second block happens before the first's
    /// timer fires, the first's timer becomes a no-op (its captured
    /// generation no longer matches) — the newer block's own timer owns the
    /// eventual revert, so the tray never flickers back to a stale display
    /// mid-block.
    fn publish_recent_block(&self, what: String) {
        let generation = self.block_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.tray_pub.set(TrayState::RecentBlock {
            what,
            ttl: RECENT_BLOCK_TTL,
        });

        let cache = self.cache.clone();
        let block_generation = self.block_generation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RECENT_BLOCK_TTL).await;
            if block_generation.load(Ordering::SeqCst) == generation {
                // Revert via the cache, which already holds the same
                // publisher and knows the actual current Idle/Pending(n)
                // state — not a hardcoded Idle.
                cache.lock().await.resync_tray_state();
            }
        });
    }
}

#[tonic::async_trait]
impl Ui for UiService {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        let req = request.into_inner();
        let id = req.id;

        // Every ping (not just ones carrying stats) counts as evidence the
        // daemon is alive — see `DaemonLiveness`'s doc comment for why this
        // is only one of several signals the bridge treats as a heartbeat.
        self.liveness.touch();

        // The daemon's periodic Ping carries `Statistics.events`: recent
        // connections it matched (and decided) against a *pre-existing*
        // rule, entirely without going through the interactive `AskRule`
        // flow above. This is the only place the bridge learns about that
        // traffic and the rule name that governed it — surface each as a
        // decided row so the Connections view's rule-match diagnostics cover
        // every connection, not just the ones the user was prompted for.
        if let Some(stats) = req.stats {
            let new_rows: Vec<_> = stats.events.iter().filter_map(event_to_row).collect();
            if !new_rows.is_empty() {
                {
                    let mut cache = self.cache.lock().await;
                    for row in &new_rows {
                        cache.insert_decided(row.clone());
                    }
                }
                if self.broadcast.receiver_count() > 0 {
                    let msg = ServerMessage::InsertConnectionRows { rows: new_rows };
                    if let Err(e) = self.broadcast.send(msg) {
                        warn!(error = %e, "ping: broadcast send failed");
                    }
                }
            }
        }

        Ok(Response::new(PingReply { id }))
    }

    async fn ask_rule(&self, request: Request<Connection>) -> Result<Response<Rule>, Status> {
        self.liveness.touch();
        let conn = request.into_inner();
        let ask_id = self.next_ask_id.fetch_add(1, Ordering::Relaxed);

        // Filtering paused (tray "Pause filtering"): auto-allow without
        // prompting. opensnitchd's own DefaultAction stays untouched — only
        // the bridge's own decision policy changes, so a genuine bridge
        // outage (this process crashing, not merely being paused) still
        // hits the daemon's fail-closed default. See
        // docs/superpowers/plans/2026-07-12-tray-filter-off.md.
        if self.filtering_paused.load(Ordering::Relaxed) {
            let row = connection_to_row(&conn, ask_id);
            let mut decided_row = row.clone();
            decided_row.action = Some("allow".to_string());
            {
                let mut cache = self.cache.lock().await;
                cache.insert_decided(decided_row.clone());
            }
            if self.broadcast.receiver_count() > 0 {
                let msg = ServerMessage::InsertConnectionRows {
                    rows: vec![decided_row],
                };
                if let Err(e) = self.broadcast.send(msg) {
                    warn!(error = %e, "ask_rule (paused): broadcast send failed");
                }
            }
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            return Ok(Response::new(verdict_to_rule(
                Verdict::Allow,
                VerdictDuration::Once,
                crate::ws_messages::VerdictScope::ThisHost,
                &conn,
                now_secs,
            )));
        }

        let row = connection_to_row(&conn, ask_id);
        // Captured before `row` moves into the broadcast message below —
        // reused for RecentBlock's tooltip if this resolves to a Deny,
        // rather than re-deriving the same process/host display logic.
        let what = format!("{} → {}", row.process, row.dst_host);
        let verdict_rx = {
            let mut cache = self.cache.lock().await;
            cache.insert_pending(row.clone())
        };

        if self.broadcast.receiver_count() > 0 {
            let msg = ServerMessage::InsertConnectionRows { rows: vec![row] };
            if let Err(e) = self.broadcast.send(msg) {
                warn!(error = %e, "broadcast send failed");
            }
        }

        // Notify desktop (Tauri shell shows a notification bubble).
        self.notice_bus.send(crate::notice::Notice::Pending {
            row_id: ask_id,
            process: conn.process_path.clone(),
        });

        let resolution = verdict_rx
            .await
            .map_err(|_canceled| Status::cancelled("verdict oneshot dropped before resolution"))?;

        if resolution.verdict == Verdict::Deny {
            self.publish_recent_block(what.clone());

            // FIX 2 (issue #14 security review): a narrowed Deny
            // under-blocks relative to what the pending-decision dialog
            // offered, so the client must be told — not left to assume the
            // wider block applied.
            if let Some(reason) = crate::translator::verdict::scope_degradation_reason(
                resolution.scope,
                resolution.verdict,
                &conn,
            ) {
                self.notice_bus
                    .send(crate::notice::Notice::DenyScopeNarrowed {
                        row_id: ask_id,
                        what,
                        reason,
                    });
            }
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Response::new(verdict_to_rule(
            resolution.verdict,
            resolution.duration,
            resolution.scope,
            &conn,
            now_secs,
        )))
    }

    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        self.liveness.touch();
        let cfg = request.into_inner();
        info!(client = %cfg.name, version = %cfg.version, "client subscribed");
        {
            let mut guard = self
                .firewall_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *guard = Some(cfg.is_firewall_running);
        }
        // Deliberately does NOT clear `alert_store` — see that field's doc
        // comment and `daemon_alerts`'s module doc for why a fresh
        // `subscribe()` is the wrong trigger for that.
        Ok(Response::new(cfg))
    }

    async fn post_alert(&self, request: Request<Alert>) -> Result<Response<MsgResponse>, Status> {
        self.liveness.touch();
        let alert = request.into_inner();
        info!(id = alert.id, type_ = alert.r#type, "alert received");

        // An unrecognized `What` value used to be coerced to `Generic` —
        // now that `Generic` is a meaningful bucket the diagnostics overlay
        // text-classifies (see `diagnostics::classify_generic_alert_text`),
        // silently relabeling a genuinely-unknown value as `Generic` would
        // feed the classifier data that was never actually reported as
        // GENERIC. Skip recording instead.
        let Ok(what) = snitchwatch_proto::protocol::alert::What::try_from(alert.what) else {
            debug!(
                what = alert.what,
                "post_alert: unrecognized What value, not recording"
            );
            return Ok(Response::new(MsgResponse { id: alert.id }));
        };
        let text = match &alert.data {
            Some(snitchwatch_proto::protocol::alert::Data::Text(text)) => Some(text.clone()),
            Some(_) => {
                debug!("post_alert: dropping non-text alert payload");
                None
            }
            None => None,
        };
        if let Some(text) = text {
            self.alert_store.record(what, alert.r#type, text);
            // Push a fresh report immediately so the GUI's diagnostics
            // banner reacts without waiting for a manual recheck. Recording
            // above already happened even if no `DiagnosticsCtx` is wired up
            // yet (e.g. most unit tests here) — only the push is skipped.
            if let Some(ctx) = self.diagnostics_ctx.get() {
                // `receiver_count() > 0` is a cosmetic short-circuit, not a
                // correctness guard: `broadcast::Sender::send` already
                // returns `Err` (silently handled below) with zero
                // receivers, and the alert is retained in `alert_store`
                // either way — a client that subscribes later still gets it
                // via the next `report()`.
                if self.broadcast.receiver_count() > 0 {
                    let msg = ServerMessage::DiagnosticsReport {
                        checks: ctx.report(),
                    };
                    if let Err(e) = self.broadcast.send(msg) {
                        warn!(error = %e, "post_alert: broadcast send failed");
                    }
                }
            }
        }

        Ok(Response::new(MsgResponse { id: alert.id }))
    }

    type NotificationsStream =
        Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send + 'static>>;

    async fn notifications(
        &self,
        request: Request<Streaming<NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        info!("notifications stream opened");
        // The stream being open is itself proof of life — see
        // `DaemonLiveness`'s doc comment for why this is the authoritative
        // signal for an idle-but-connected daemon. `StreamGuard` ties the
        // decrement to Drop (not just the loop's normal exit) so a panic
        // partway through the reply loop can't wedge the counter open
        // forever — see `StreamGuard`'s doc comment.
        let guard = StreamGuard::open(self.liveness.clone());
        let liveness = self.liveness.clone();
        let mut inbound = request.into_inner();
        tokio::spawn(async move {
            let _guard = guard;
            while let Ok(Some(reply)) = inbound.message().await {
                liveness.touch();
                info!(
                    id = reply.id,
                    code = reply.code,
                    "notification reply from daemon"
                );
            }
            warn!("notification reply stream ended");
            // `_guard` drops here (or during an unwind, if the loop above
            // ever panics), closing the stream.
        });

        let outbound = async_stream::try_stream! {
            // Hold the stream open with no commands until M3+ wires up
            // config-push from the GUI side.
            let () = std::future::pending().await;
            yield Notification::default();
        };

        Ok(Response::new(
            Box::pin(outbound) as Self::NotificationsStream
        ))
    }
}

#[cfg(test)]
#[path = "grpc_server/tests.rs"]
mod tests;
