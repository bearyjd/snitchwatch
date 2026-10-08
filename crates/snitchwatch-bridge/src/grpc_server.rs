//! Bridge-side gRPC server: implements `protocol.UI` and is dialed by
//! opensnitchd as the gRPC client.
//!
//! Replaces the M1 dial-out flow that lived in the now-deleted
//! `grpc_client.rs` and `translator/downstream.rs` envelope hack.

use crate::cache::connections::{ConnectionCache, Verdict, VerdictResolution};
use crate::cache::rules::{RulesSync, SharedRulesCache};
use crate::client_presence::ClientPresence;
use crate::daemon_alerts::DaemonAlertStore;
use crate::daemon_commands::{DaemonCommands, DaemonTransport};
use crate::daemon_liveness::StreamGuard;
use crate::diagnostics::DiagnosticsCtx;
use crate::filter_pause::FilterPause;
use crate::notice::NoticeBus;
use crate::rule_wire::rule_to_wire;
use crate::translator::connection::{connection_to_row, event_to_row};
use crate::translator::verdict::{once_rule, verdict_to_rule};
use crate::tray_state::{TrayState, TrayStatePublisher};
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Action, Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply,
    PingReply, PingRequest, Rule,
};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Largest daemon message decoded (tonic's own default, made explicit). The
/// biggest is a `Subscribe` carrying the full rule list, which
/// `cache::rules::MAX_SNAPSHOT_RULES` bounds again after decoding.
pub(crate) const MAX_DAEMON_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

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
    /// The user's timed filtering pause (tray "Pause filtering", issue #47).
    /// Unlike `liveness`/`block_generation`, this must be *writable* from
    /// outside `UiService` (the inbound `ClientMessage` pump sets it through
    /// `client_presence::apply_pause_request`) as well as readable from
    /// inside `ask_rule` — the same shape `tray_pub`/`cache` already have —
    /// so it's a genuine constructor parameter, not internal-only state. See
    /// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`.
    filter_pause: Arc<FilterPause>,
    client_presence: ClientPresence,
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
    /// Outbound rule commands and reply correlation (`daemon_commands`);
    /// internal for the same reason as [`Self::diagnostics_ctx`].
    commands: DaemonCommands,
    /// The daemon's rule list (issue #48): staged per connection by
    /// `subscribe`, committed on that connection's HELLO.
    rules: RulesSync,
    /// Who holds the daemon's single prompt slot (`crate::prompt_slot`).
    prompt_slot: crate::prompt_slot::PromptSlotHandle,
}

/// Future-drop cleanup also runs for tonic transport cancellation. A closed
/// receiver makes late verdicts fail even while asynchronous cleanup waits
/// for the cache mutex; a settled verdict is never removed.
struct PendingCleanup {
    cache: Arc<Mutex<ConnectionCache>>,
    row_id: String,
    slot: crate::prompt_slot::PromptSlotHandle,
    ask_id: u64,
}

impl PendingCleanup {
    /// Marks the prompt as holding the slot. Every exit from here, a dropped
    /// future included, releases it in `drop`.
    fn hold(service: &UiService, row_id: String, ask_id: u64, what: String) -> Self {
        service.prompt_slot.hold(&row_id, what);
        Self {
            cache: service.cache.clone(),
            row_id,
            slot: service.prompt_slot.clone(),
            ask_id,
        }
    }
}

impl Drop for PendingCleanup {
    fn drop(&mut self) {
        self.slot.release(&self.row_id, self.ask_id);
        if let Ok(mut cache) = self.cache.try_lock() {
            cache.cancel_pending(&self.row_id);
        } else {
            let cache = self.cache.clone();
            let row_id = self.row_id.clone();
            tokio::spawn(async move {
                cache.lock().await.cancel_pending(&row_id);
            });
        }
    }
}

impl UiService {
    pub fn new(
        cache: Arc<Mutex<ConnectionCache>>,
        broadcast: broadcast::Sender<ServerMessage>,
        tray_pub: Arc<TrayStatePublisher>,
        notice_bus: Arc<NoticeBus>,
        filter_pause: Arc<FilterPause>,
    ) -> Self {
        let rules = RulesSync::new(broadcast.clone());
        let prompt_slot =
            crate::prompt_slot::PromptSlotHandle::new(broadcast.clone(), notice_bus.clone());
        Self {
            cache,
            broadcast,
            next_ask_id: Arc::new(AtomicU64::new(1)),
            tray_pub,
            notice_bus,
            liveness: DaemonLiveness::new(),
            block_generation: Arc::new(AtomicU64::new(0)),
            filter_pause,
            client_presence: ClientPresence::default(),
            firewall_status: Arc::new(StdMutex::new(None)),
            alert_store: Arc::new(DaemonAlertStore::new()),
            diagnostics_ctx: Arc::new(OnceLock::new()),
            commands: DaemonCommands::new(DaemonTransport::Tcp, rules.clone()),
            rules,
            prompt_slot,
        }
    }

    /// The prompt slot, for the `RequestSnapshot` answer.
    pub fn prompt_slot_handle(&self) -> crate::prompt_slot::PromptSlotHandle {
        self.prompt_slot.clone()
    }

    pub fn with_client_presence(mut self, presence: ClientPresence) -> Self {
        self.client_presence = presence;
        self
    }

    pub fn client_presence(&self) -> ClientPresence {
        self.client_presence.clone()
    }

    /// TCP (the default) fans commands out to every open daemon stream; the
    /// root-only Unix socket uses the current one. Call before taking handles.
    pub fn with_daemon_transport(mut self, transport: DaemonTransport) -> Self {
        self.commands = DaemonCommands::new(transport, self.rules.clone());
        self
    }

    /// Outbound rule commands (see [`Self::commands`]).
    pub fn daemon_commands(&self) -> DaemonCommands {
        self.commands.clone()
    }

    /// The bridge's copy of the daemon's rule list.
    pub fn rules_handle(&self) -> SharedRulesCache {
        self.rules.cache()
    }

    /// Generation bumped each time a daemon rule snapshot is committed: the
    /// blocklist reconcile trigger (issue #45).
    pub fn rules_synced(&self) -> tokio::sync::watch::Receiver<u64> {
        self.rules.synced()
    }

    /// Convenience: wrap into a tonic `UiServer<UiService>` ready for
    /// `Server::builder().add_service(...)`.
    /// The decode limit is explicit: it bounds one `Subscribe` rule snapshot.
    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self).max_decoding_message_size(MAX_DAEMON_MESSAGE_BYTES)
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

    /// The `AskRule` reply for a resolved verdict.
    ///
    /// A one-shot reply is deliberately absent from Rules. Every remembered
    /// rule is an active daemon rule, including a five-minute or
    /// until-restart rule, and must be visible/editable immediately rather
    /// than waiting for a daemon-side rule-list push that may never come.
    /// May diverge on the daemon's `setUniqueName`; see `cache::rules`.
    ///
    /// Issue #44: a remembered verdict `verdict_to_rule` refuses (no absolute
    /// process path) is answered once instead, never cached or announced as
    /// a rule, and every client is told why.
    fn verdict_reply(
        &self,
        resolution: VerdictResolution,
        conn: &Connection,
        row_id: String,
        ask_id: u64,
        now_secs: i64,
    ) -> Rule {
        let refusal = match verdict_to_rule(
            resolution.verdict,
            resolution.duration,
            resolution.scope,
            conn,
            now_secs,
        ) {
            Ok(rule) => {
                if resolution.duration.remembers() {
                    self.rules.upsert(rule.clone());
                    if self.broadcast.receiver_count() > 0 {
                        if let Err(e) = self.broadcast.send(ServerMessage::UpdateRules {
                            rules: vec![rule_to_wire(&rule)],
                        }) {
                            warn!(error = %e, "persistent verdict rule broadcast failed");
                        }
                    }
                }
                return rule;
            }
            Err(refusal) => refusal,
        };

        if self.broadcast.receiver_count() > 0 {
            if let Err(e) = self.broadcast.send(ServerMessage::VerdictNotRemembered {
                row_id,
                reason: refusal.describe().to_string(),
            }) {
                warn!(error = %e, "verdict-not-remembered broadcast send failed");
            }
        }
        self.notice_bus
            .send(crate::notice::Notice::VerdictNotRemembered { row_id: ask_id });
        once_rule(resolution.verdict, resolution.scope, conn, now_secs)
    }

    /// Publish `TrayState::RecentBlock` and schedule its own revert after
    /// [`RECENT_BLOCK_TTL`]. If a second block happens before the first's
    /// timer fires, the first's timer becomes a no-op (its captured
    /// generation no longer matches) — the newer block's own timer owns the
    /// eventual revert, so the tray never flickers back to a stale display
    /// mid-block.
    ///
    /// Not while the daemon is down (issue #58): the overlay would cover
    /// `DaemonDown` for the whole TTL. The check and the publish happen under
    /// the cache lock, which is also what the daemon watchdog holds when it
    /// marks the daemon down and publishes, so neither can interleave.
    async fn publish_recent_block(&self, what: String) {
        let generation = {
            let cache = self.cache.lock().await;
            if cache.tray_state() == TrayState::DaemonDown {
                return;
            }
            // Numbered in publish order, under the lock.
            let generation = self.block_generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.tray_pub.set(TrayState::RecentBlock {
                what,
                ttl: RECENT_BLOCK_TTL,
            });
            generation
        };

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
            self.prompt_slot.observe(
                stats.rule_misses,
                stats.uptime,
                *self.rules.synced().borrow(),
            );
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

            // Aggregate counters from the same `Statistics` payload (issue
            // #19). Not gated on `new_rows` being non-empty — `stats.events`
            // and the scalar counters are independent fields on the same
            // message, so a `Statistics` with only scalars still broadcasts.
            // Note the daemon itself, not this bridge, controls *whether* a
            // `Statistics` payload is sent at all: `Serialize()` returns
            // `nil` when it has no new events since the last ping
            // (`vendor/opensnitch/daemon/statistics/stats.go:266`), and the
            // client then skips the Ping RPC entirely
            // (`vendor/opensnitch/daemon/client.go:337-341`) — so on an idle
            // system with no new connection activity, the daemon simply
            // doesn't ping, and these counters (uptime included) can go
            // stale until the next one. This broadcast itself stays
            // unconditional on `stats` being present, defensively, in case
            // that upstream gating ever changes.
            if self.broadcast.receiver_count() > 0 {
                let stats_msg = ServerMessage::DaemonStatistics {
                    daemon_version: crate::translator::verdict::sanitize_for_display(
                        &stats.daemon_version,
                        32,
                    ),
                    uptime: stats.uptime,
                    rules: stats.rules,
                    connections: stats.connections,
                    ignored: stats.ignored,
                    accepted: stats.accepted,
                    dropped: stats.dropped,
                    rule_hits: stats.rule_hits,
                    rule_misses: stats.rule_misses,
                };
                if let Err(e) = self.broadcast.send(stats_msg) {
                    warn!(error = %e, "ping: daemon statistics broadcast send failed");
                }
            }
        }

        Ok(Response::new(PingReply { id }))
    }

    async fn ask_rule(&self, request: Request<Connection>) -> Result<Response<Rule>, Status> {
        self.liveness.touch();
        let conn = request.into_inner();
        let ask_id = self.next_ask_id.fetch_add(1, Ordering::Relaxed);

        // Checked before the pause shortcut: a pause is a GUI user's choice
        // and must not outlive every GUI session. With no authenticated GUI
        // the daemon applies its own default action, paused or not
        // (security review 2026-10-07, issue #47).
        let mut admission = self
            .client_presence
            .admit()
            .ok_or_else(|| Status::unavailable("no authenticated GUI session"))?;

        // Filtering paused (tray "Pause filtering"): auto-allow (Once)
        // without prompting; the daemon's DefaultAction is untouched, so a
        // crashed bridge still hits it (plans/2026-07-12-tray-filter-off.md).
        // `applies_to` checks the deadline (an expired pause stops at once)
        // and that this admission's GUI-session generation set the pause,
        // under the presence lock (issue #47).
        if self.filter_pause.applies_to(&admission) {
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
            return Ok(Response::new(once_rule(
                Verdict::Allow,
                crate::ws_messages::VerdictScope::ThisHost,
                &conn,
                now_secs,
            )));
        }

        let row = connection_to_row(&conn, ask_id);
        // Captured before `row` moves into the broadcast message below.
        // Both `row.process` and `row.dst_host` are attacker-influenced
        // (`row.process` is the basename of `process_path`: daemon-attested
        // *existence*, but a local user still picks the path/basename text
        // itself, e.g. `/tmp/<b>evil</b>` or a binary named with an ANSI
        // escape — not the "safe, daemon-attested" class
        // `translator::verdict`'s module doc means by `process_path` being
        // trusted; only its *authenticity* as "this really is what ran" is
        // trusted, not its byte content). Every display-bound consumer —
        // the WS degradation notice, notification bodies, and the tray
        // `RecentBlock` tooltip (issue #15) — must get the sanitized form.
        let safe_what = display_summary(&row.process, &row.dst_host);
        let slot_what = crate::prompt_slot::plain_summary(&row.process, &row.dst_host);
        let row_id = row.id.clone();
        let verdict_rx = {
            let mut cache = self.cache.lock().await;
            let receiver = cache
                .insert_admitted(row.clone(), admission.clone(), self.broadcast.clone())
                .ok_or_else(|| {
                    Status::unavailable("authenticated GUI session lost before admission")
                })?;
            // Publish insertion under the settlement mutex so cancellation
            // cannot publish removal first and leave a stale prompt behind.
            let _ = self
                .broadcast
                .send(ServerMessage::InsertConnectionRows { rows: vec![row] });
            // Every desktop notifier puts this in a body that freedesktop
            // servers render as markup (#44 security review S3).
            self.notice_bus.send(crate::notice::Notice::Pending {
                row_id: ask_id,
                process: crate::translator::verdict::sanitize_for_display(&conn.process_path, 128),
            });
            receiver
        };
        let _pending_cleanup = PendingCleanup::hold(self, row_id.clone(), ask_id, slot_what);
        // Declared after cleanup so cancellation drops the receiver first.
        let mut verdict_rx = verdict_rx;

        let resolution = tokio::select! {
            resolution = &mut verdict_rx => resolution,
            _ = admission.lost() => {
                if self.cache.lock().await.cancel_pending(&row_id) {
                    return Err(Status::unavailable("last authenticated GUI session disconnected"));
                }
                // A verdict serialized before the loss already settled this Ask.
                verdict_rx.await
            }
        }
        .map_err(|_| Status::unavailable("pending Ask cancelled before resolution"))?;

        if resolution.verdict == Verdict::Deny {
            self.publish_recent_block(safe_what.clone()).await;

            // FIX 2 (issue #14 security review): a narrowed Deny
            // under-blocks relative to what the pending-decision dialog
            // offered, so the client must be told — not left to assume the
            // wider block applied. `degradation.describe()` is always a
            // fixed, safe string (see `ScopeDegradation`'s doc comment) —
            // only `safe_what` (the process/host summary) needed
            // display-boundary sanitization.
            if let Some(degradation) = crate::translator::verdict::scope_degradation(
                resolution.scope,
                resolution.verdict,
                &conn,
            ) {
                let reason = degradation.describe().to_string();

                // Round 2 of the issue #14 security review (HIGH): this
                // MUST reach the WS client as a real protocol message, not
                // only the desktop `Notice` bus below — that bus is
                // consumed solely by the desktop notifiers, so a headless
                // `bridge-cli`, an unattended GUI, or a session with no
                // D-Bus notification server previously got no signal at
                // all that the block had been narrowed.
                if self.broadcast.receiver_count() > 0 {
                    let msg = ServerMessage::DenyScopeNarrowed {
                        row_id: row_id.clone(),
                        reason: reason.clone(),
                    };
                    if let Err(e) = self.broadcast.send(msg) {
                        warn!(error = %e, "deny-scope-narrowed broadcast send failed");
                    }
                }

                self.notice_bus
                    .send(crate::notice::Notice::DenyScopeNarrowed {
                        row_id: ask_id,
                        what: safe_what,
                        reason,
                    });
            }
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Response::new(self.verdict_reply(
            resolution, &conn, row_id, ask_id, now_secs,
        )))
    }

    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        self.liveness.touch();
        let conn = request.remote_addr();
        let cfg = request.into_inner();
        info!(client = %cfg.name, version = %cfg.version, "client subscribed");
        // Staged until this connection's stream says HELLO (see `cache::rules`).
        self.rules.stage(conn, cfg.rules.clone());
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
        // Registered before the reply loop, so HELLO can't race it; dropped on loop end/unwind.
        let (registration, mut rx) = self.commands.open_stream(request.remote_addr());
        let service = self.clone();
        let mut inbound = request.into_inner();
        tokio::spawn(async move {
            let _guard = guard;
            while let Ok(Some(reply)) = inbound.message().await {
                service.liveness.touch();
                info!(
                    id = reply.id,
                    code = reply.code,
                    "notification reply from daemon"
                );
                service.commands.on_reply(registration.id(), &reply);
            }
            warn!("notification reply stream ended");
            // `_guard` drops here (or during an unwind, if the loop above
            // ever panics), closing the stream.
        });

        // Relay outbound commands (rule enable/disable/delete) to this daemon.
        //
        // NEVER yield `Notification::default()`. Its `type` is `Action::None`
        // (0), and the daemon treats `ntf.Type <= Action_NONE` as "server
        // ordered to close notifications" and tears the stream down
        // (`vendor/opensnitch/daemon/ui/notifications.go:405-408`). The
        // placeholder this replaced would have done exactly that. Producers go
        // through `daemon_commands()`, and `Action::None` is filtered here
        // as a second line of defence. Ends when the stream is closed.
        let outbound = async_stream::try_stream! {
            while let Some(notification) = rx.recv().await {
                if notification.r#type == Action::None as i32 {
                    warn!(
                        id = notification.id,
                        "refusing to send a NONE-typed notification; it would close \
                         the daemon's stream"
                    );
                    continue;
                }
                debug!(
                    id = notification.id,
                    action = notification.r#type,
                    rules = notification.rules.len(),
                    "sending notification to daemon"
                );
                yield notification;
            }
        };

        Ok(Response::new(
            Box::pin(outbound) as Self::NotificationsStream
        ))
    }
}

/// Display-boundary "process → host" summary for notifications and the tray
/// `RecentBlock` tooltip. Both inputs are attacker-influenced text (see the
/// call site in [`Ui::ask_rule`]) and must pass through
/// [`sanitize_for_display`](crate::translator::verdict::sanitize_for_display)
/// before reaching any UI surface.
pub(crate) fn display_summary(process: &str, dst_host: &str) -> String {
    format!(
        "{} → {}",
        crate::translator::verdict::sanitize_for_display(process, 64),
        crate::translator::verdict::sanitize_for_display(dst_host, 64)
    )
}

#[cfg(test)]
#[path = "grpc_server/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "grpc_server/refusal_tests.rs"]
mod refusal_tests;

#[cfg(test)]
#[path = "grpc_server/prompt_slot_tests.rs"]
mod prompt_slot_tests;
