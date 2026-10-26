//! `NotificationController` — desktop notification dispatch (Task 17).
//!
//! **Dispatch mechanism.** The plan's primary path was `KNotification` via
//! `cxx-kde-frameworks`; verifying that crate's actual coverage was part of
//! this task. It wraps KDE Frameworks' C++ classes selectively and does not
//! expose `KNotification`'s send path, so building on it here would mean
//! writing untested/unverified binding surface for a shell-chrome feature.
//! The pragmatic accepted path — explicitly sanctioned by the plan as a
//! fallback — is `notify-rust`, which is **already a proven workspace
//! dependency**: `snitchwatch-tauri::notifier` dispatches through it today
//! (`notify-rust = "4"`, default `z` feature → `zbus` 5.x, both already in
//! `Cargo.lock`). Reusing it here adds no new supply-chain surface and both
//! `notify-rust` and raw D-Bus ultimately speak the same
//! `org.freedesktop.Notifications` spec `KNotification` also targets, so
//! Plasma's action-button rendering and Do-Not-Disturb handling apply either
//! way.
//!
//! **This GUI's own prompts (LS redesign phase 1).** The alert window is
//! independent of the main window, so a prompt of the live session is
//! answered by `silent_mode` (Allow/Deny: no notice, ever) or asked by the
//! alert ("Ask me"). Only if the alert is not the active window once the
//! grace period ends (`alertActive`, set from `AlertHost.qml`), a notice is
//! the fallback, so an alert nobody saw is not a prompt nobody saw. Other
//! sessions' prompts are gated on the main window as before.
//!
//! **Cooldown.** [`crate::notifier::CooldownGate`], extended in 0d with a
//! one-hour repeat window per flow.
//!
//! **5-second pending grace period.** Per the original design spec's
//! "Pending decision" notification rule ("only if the Snitchwatch window is
//! hidden AND the row has been pending for more than 5 seconds"): a
//! `Notice::Pending` is not dispatched immediately. A 5-second delay timer is
//! started instead; if the main window is still not active when it fires,
//! *then* the cooldown-gated notification goes out with a "Review" action.
//! If the window is active then, it is looked at again every second while
//! the row waits, so hiding the window later still posts the notice (r11
//! issue A: a new prompt raises the window, `main.qml` Task 7, so it is
//! usually active when the grace period ends).
//! `DaemonAway`/`FilterPauseExpired` are not window-gated (matches the
//! original `notifier.rs`, which never gated on window visibility for those).
//! Neither is `VerdictNotRemembered` (issue #44): `ConnectionsPage.qml`'s
//! in-app message exists only while that page is the current one.
//!
//! **"Review" action → raise window.** The pending notice's buttons are
//! heard on the bridge runtime, by `crate::pending_notice` through
//! `crate::notification_signals::Notice::wait`, which trusts only the
//! notification server. A "Review" click is queued back as the
//! `reviewRequested` signal, which `main.qml` connects to the same
//! raise/`requestActivate()` call Task 7's pending-count handler uses.
//!
//! **"Allow once" and "Deny" (prompt-slot plan Part B).** A pending notice
//! also answers the prompt, as the inline buttons would
//! (`crate::notification_actions`). It is shown only while its row still
//! waits in the same session, checked after the grace period, and each
//! action checks again. Its body is built from that row, escaped. It is
//! sent and heard through `crate::notification_signals`, not notify-rust,
//! so only the notification server can click it (`crate::pending_notice`).
//! Every other notice has no actions.

use core::pin::Pin;
use cxx_qt::Threading;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::bridge_runtime::{BridgeHandles, BridgeNotice, HeldBackNotices, ReceivedNotice};
use crate::notifier::{CooldownGate, FlowDecision, FlowKey};
use crate::pending_notice::{show_and_answer, show_plain, PendingTarget};
use crate::silent_mode;
use snitchwatch_bridge::translator::process_binding::RuleRefusal;

/// Per the design spec: a pending row is only worth a fallback desktop
/// notification once it has been waiting this long with the window hidden.
const PENDING_GRACE_PERIOD: Duration = Duration::from_secs(5);
/// How often a waiting prompt whose notice is held back by the active
/// window is looked at again, so hiding the window later still posts it.
const WINDOW_RECHECK: Duration = Duration::from_secs(1);

/// One look, on the Qt thread, at a waiting prompt's notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingLook {
    /// The window is active: the prompt is in front of the user there.
    WindowActive,
    /// Posted, or held back by the cooldown: nothing more to do.
    Done,
    /// The row no longer waits in its session: nothing to post.
    Gone,
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Desktop notification dispatcher, bound by `main.qml`.
        #[qobject]
        #[qml_element]
        /// Set by `main.qml` from `root.active`; used to gate the
        /// `Notice::Pending` fallback notification (never suppresses
        /// `DaemonAway`/`FilterPauseExpired`). Defaults to `true` (assume
        /// visible/focused) so a brief startup race before QML wires the
        /// binding fails toward *not* spamming a notification, not toward
        /// spuriously firing one.
        #[qproperty(bool, window_active, cxx_name = "windowActive")]
        /// Set by `AlertHost.qml`: the alert window is shown and has focus.
        /// Gates the fallback notice for a prompt of this GUI's own session
        /// (which `windowActive` says nothing about). Defaults to `false`:
        /// unknown means the alert was not seen.
        #[qproperty(bool, alert_active, cxx_name = "alertActive")]
        type NotificationController = super::NotificationControllerRust;

        /// Emitted when the user clicks a notification's "Review" action.
        /// `main.qml` connects this to the same window-raise call Task 7's
        /// pending-count handler uses.
        #[qsignal]
        #[cxx_name = "reviewRequested"]
        fn review_requested(self: Pin<&mut NotificationController>);

        /// Start the live feed: subscribe to the bridge's notice broadcast
        /// and dispatch cooldown-gated desktop notifications. No-op when the
        /// bridge isn't running. Called from `main.qml`'s
        /// `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut NotificationController>);
    }

    impl cxx_qt::Threading for NotificationController {}
}

/// Rust-side state for [`qobject::NotificationController`].
pub struct NotificationControllerRust {
    window_active: bool,
    alert_active: bool,
}

impl Default for NotificationControllerRust {
    fn default() -> Self {
        Self {
            window_active: true,
            alert_active: false,
        }
    }
}

impl qobject::NotificationController {
    fn start_bridge_feed(self: Pin<&mut Self>) {
        let (Some(notice_rx), Some(handles)) = (
            crate::bridge_runtime::notice_rx(),
            crate::bridge_runtime::handles(),
        ) else {
            tracing::warn!("NotificationController: bridge not running; live feed disabled");
            return;
        };
        // Shared across every notice this feed ever sees (including the
        // delayed Pending checks), so the 30s-default cooldown is tracked
        // per `NoticeKey` across the whole feed's lifetime, not reset per
        // notice.
        let gate = Arc::new(Mutex::new(CooldownGate::new()));
        let runtime = handles.runtime().clone();
        runtime.spawn(relay_notices(notice_rx, handles, self.qt_thread(), gate));
    }
}

type QtThread = cxx_qt::CxxQtThread<qobject::NotificationController>;

/// Forward every bridge notice to the Qt thread until the feed closes.
async fn relay_notices(
    mut notice_rx: broadcast::Receiver<ReceivedNotice>,
    handles: BridgeHandles,
    qt_thread: QtThread,
    gate: Arc<Mutex<CooldownGate>>,
) {
    loop {
        match notice_rx.recv().await {
            Ok(received) => relay(received, &handles, &qt_thread, &gate),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    skipped = n,
                    "NotificationController feed lagged behind bridge"
                )
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// One notice. A Pending one waits out the grace period first, spawned so
/// a burst of them doesn't stall DaemonAway/FilterPauseExpired, and is
/// shown only while its row still waits in its session (Part B). A prompt
/// of this GUI's own session is held to the alert window instead of the
/// main one, and never announced in silent allow/deny modes.
fn relay(
    received: ReceivedNotice,
    handles: &BridgeHandles,
    qt_thread: &QtThread,
    gate: &Arc<Mutex<CooldownGate>>,
) {
    let ReceivedNotice {
        connection_id,
        notice,
    } = received;
    let (handles, qt_thread, gate) = (handles.clone(), qt_thread.clone(), gate.clone());
    let BridgeNotice::Pending { row_id, .. } = notice else {
        let _ = qt_thread.queue(move |qobject| {
            if handles.is_current_session(connection_id) {
                qobject.maybe_dispatch(notice, gate, None, true);
            }
        });
        return;
    };
    tokio::spawn(async move {
        let look = || {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let (handles, notice, gate) = (handles.clone(), notice.clone(), gate.clone());
            let queued = qt_thread.queue(move |qobject| {
                let current = handles.is_current_session(connection_id);
                let look = if current && silent_mode::current_choice().is_some() {
                    // Silent mode answers it; the user chose not to be asked.
                    PendingLook::Gone
                } else {
                    match PendingTarget::of(&handles, connection_id, &notice) {
                        Some(target) => qobject.maybe_dispatch(notice, gate, Some(target), current),
                        None => PendingLook::Gone,
                    }
                };
                let _ = tx.send(look);
            });
            async move {
                match queued {
                    Ok(()) => rx.await.unwrap_or(PendingLook::Gone),
                    Err(_) => PendingLook::Gone,
                }
            }
        };
        post_once_the_window_is_not_active(connection_id, row_id, look).await;
    });
}

/// r11 issue A: a prompt raises the window (`main.qml`, Task 7), so after
/// the grace period the window is usually active and no notice is posted.
/// That used to be the only look, so closing the window later, with the
/// prompt still waiting, never posted one. Now, after the grace period,
/// `look` is asked every [`WINDOW_RECHECK`] while the window is active,
/// until the notice is posted or its row stops waiting.
async fn post_once_the_window_is_not_active<F, Fut>(session: u64, row_id: u64, mut look: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = PendingLook>,
{
    tokio::time::sleep(PENDING_GRACE_PERIOD).await;
    let mut held_back = false;
    loop {
        match look().await {
            PendingLook::WindowActive => {
                if !held_back {
                    tracing::debug!(
                        session,
                        row_id,
                        "pending notice held back: the window is active; it is posted if the \
                         window is hidden or loses focus while the prompt still waits"
                    );
                    held_back = true;
                }
                tokio::time::sleep(WINDOW_RECHECK).await;
            }
            PendingLook::Done => return,
            PendingLook::Gone => {
                tracing::debug!(
                    session,
                    row_id,
                    held_back,
                    "no pending notice: the prompt no longer waits"
                );
                return;
            }
        }
    }
}

/// Whether what the user is looking at already shows a waiting prompt: the
/// alert window for this GUI's own session, the main window otherwise.
fn front_is_active(current_session: bool, window_active: bool, alert_active: bool) -> bool {
    if current_session {
        alert_active
    } else {
        window_active
    }
}

/// Whether a notice may go out now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Post,
    /// A Pending notice while the window is active.
    WindowActive,
    /// The same notice went out within the cooldown.
    CooledDown,
    /// A repeat of a flow whose notice was posted within the repeat window.
    Repeat,
}

/// A Pending notice doesn't go out while the window is active, and no
/// notice goes out twice within the cooldown. The cooldown, and the flow's
/// posted-notice entry, are taken only when it goes out, so a notice held back by
/// the window can go later.
pub(crate) fn may_post(
    notice: &BridgeNotice,
    flow: Option<&FlowKey>,
    window_active: bool,
    gate: &Mutex<CooldownGate>,
    now: Instant,
) -> Gate {
    if matches!(notice, BridgeNotice::Pending { .. }) && window_active {
        return Gate::WindowActive;
    }
    let decision = gate
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .decide_for_flow(notice, flow, now);
    match decision {
        FlowDecision::Fire => Gate::Post,
        FlowDecision::Repeat => Gate::Repeat,
        FlowDecision::CooledDown => Gate::CooledDown,
    }
}

/// Copy the gate's held-back count for `flow` to the rows' copy (0d part
/// 2), after every [`may_post`] for that flow, whatever it decided: a Post
/// starts the count at 0, a Repeat raises it. The gate's lock is released
/// before the copy's is taken; the epoch read with the count keeps a clear
/// that lands in between from being undone.
pub(crate) fn record_held_back(
    gate: &Mutex<CooldownGate>,
    flow: &FlowKey,
    notices: &HeldBackNotices,
    now: Instant,
) {
    let held = gate
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .held_back(flow);
    notices.record(flow, held, now);
}

/// Ends a flow's posted-notice entry when dropped, unless it is told to
/// hold: only a notice that ran its course (its row stopped waiting, or
/// the user dismissed it) keeps its repeats coalesced; whatever
/// else ends the task (answered, voided, not shown, no session bus,
/// aborted) lets the flow's next ask notify. Its held-back count goes with it.
struct ClearFlowOnDrop {
    gate: Arc<Mutex<CooldownGate>>,
    notices: HeldBackNotices,
    flow: FlowKey,
    hold: bool,
}

impl ClearFlowOnDrop {
    fn new(gate: Arc<Mutex<CooldownGate>>, notices: HeldBackNotices, flow: FlowKey) -> Self {
        Self {
            gate,
            notices,
            flow,
            hold: false,
        }
    }

    /// Keep the entry past the drop, for the rest of the repeat window.
    fn hold(&mut self) {
        self.hold = true;
    }
}

impl Drop for ClearFlowOnDrop {
    fn drop(&mut self) {
        if self.hold {
            return;
        }
        let cleared = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear_flow(&self.flow);
        // The gate's lock is released above, before this one is taken.
        self.notices.record(&self.flow, cleared, Instant::now());
    }
}

/// What a look at the notice reports for a gate result. Only the window
/// holds the notice back to be looked at again; a Repeat or a cooldown
/// touches no row and no banner.
pub(crate) fn look_for(gate: Gate) -> PendingLook {
    match gate {
        Gate::WindowActive => PendingLook::WindowActive,
        Gate::Post | Gate::CooledDown | Gate::Repeat => PendingLook::Done,
    }
}

/// A notice's kind and row, for a log line. Never its program, path, host
/// or text: those are connection details.
fn notice_label(notice: &BridgeNotice) -> (&'static str, Option<u64>) {
    match notice {
        BridgeNotice::Pending { row_id, .. } => ("pending", Some(*row_id)),
        BridgeNotice::DaemonAway => ("daemon_away", None),
        BridgeNotice::FilterPauseExpired => ("filter_pause_expired", None),
        BridgeNotice::DenyScopeNarrowed { row_id, .. } => ("deny_scope_narrowed", Some(*row_id)),
        BridgeNotice::VerdictNotRemembered { row_id } => ("verdict_not_remembered", Some(*row_id)),
        BridgeNotice::PromptSlotSummary { row_id, .. } => ("prompt_slot_summary", Some(*row_id)),
    }
}

impl qobject::NotificationController {
    /// Runs on the Qt thread (queued from the feed task above). Applies the
    /// window gate (Pending only) and the cooldown gate ([`may_post`]),
    /// then posts the notice if both allow it.
    fn maybe_dispatch(
        mut self: Pin<&mut Self>,
        notice: BridgeNotice,
        gate: Arc<Mutex<CooldownGate>>,
        target: Option<PendingTarget>,
        current_session: bool,
    ) -> PendingLook {
        let front_active =
            front_is_active(current_session, *self.window_active(), *self.alert_active());
        let flow = target.as_ref().map(PendingTarget::flow_key);
        let now = Instant::now();
        let verdict = may_post(&notice, flow.as_ref(), front_active, &gate, now);
        if let (Some(flow), Some(target)) = (&flow, &target) {
            record_held_back(&gate, flow, target.handles().held_back_notices(), now);
        }
        match verdict {
            Gate::WindowActive => {}
            Gate::CooledDown => {
                let (kind, row_id) = notice_label(&notice);
                tracing::info!(kind, ?row_id, "notice held back by its cooldown");
            }
            Gate::Repeat => {
                let (kind, row_id) = notice_label(&notice);
                tracing::info!(kind, ?row_id, "notice held back: its flow's notice is live");
            }
            Gate::Post => match target {
                Some(target) => {
                    tracing::info!("posting the pending notice: the window isn't active");
                    self.as_mut().dispatch_pending(target, gate);
                }
                None => dispatch(notice),
            },
        }
        look_for(verdict)
    }

    /// The pending notice, on the bridge runtime (`crate::pending_notice`).
    /// "Review" raises the window through `reviewRequested`.
    fn dispatch_pending(
        self: Pin<&mut Self>,
        target: PendingTarget,
        gate: Arc<Mutex<CooldownGate>>,
    ) {
        let qt_thread = self.qt_thread();
        let runtime = target.handles().runtime().clone();
        // Made here, not in the future, so a future dropped before its first
        // poll clears the flow too.
        let notices = target.handles().held_back_notices().clone();
        let live = ClearFlowOnDrop::new(gate, notices, target.flow_key());
        runtime.spawn(async move {
            let mut live = live;
            let conn = match zbus::Connection::session().await {
                Ok(conn) => conn,
                Err(error) => {
                    tracing::warn!(%error, "no session bus for the pending notification");
                    return;
                }
            };
            let end = show_and_answer(&conn, target, move || {
                let _ = qt_thread.queue(|qobject| qobject.review_requested());
            })
            .await;
            if end.holds_the_repeat_window() {
                live.hold();
            }
        });
    }
}

/// Every other notice: fixed text, no actions, on a scratch thread
/// (`notify-rust`'s `show` blocks).
fn dispatch(notice: BridgeNotice) {
    let Some((summary, body)) = notice_text(&notice) else {
        return;
    };
    std::thread::spawn(move || show_plain(summary, &body));
}

/// Summary and body of a notice with no actions; `None` for a Pending one,
/// which goes through `crate::pending_notice` instead.
fn notice_text(notice: &BridgeNotice) -> Option<(&'static str, String)> {
    let text = match notice {
        BridgeNotice::Pending { .. } => return None,
        BridgeNotice::DaemonAway => (
            "Snitchwatch — daemon unreachable",
            "opensnitchd has been unreachable for 30 seconds.".to_string(),
        ),
        BridgeNotice::FilterPauseExpired => (
            "Snitchwatch — filtering resumed",
            "Your pause timer expired.".to_string(),
        ),
        BridgeNotice::DenyScopeNarrowed { what, reason, .. } => (
            "Snitchwatch — block narrowed",
            format!("Blocked {what} for this host only — {reason}."),
        ),
        BridgeNotice::VerdictNotRemembered { .. } => (
            "Snitchwatch — answer not remembered",
            RuleRefusal::ProcessFileUnknown.describe().to_string(),
        ),
        BridgeNotice::PromptSlotSummary { count, .. } => (
            "Snitchwatch — while a prompt was open",
            snitchwatch_bridge::notice::prompt_slot_summary_text(*count),
        ),
    };
    Some(text)
}

#[cfg(test)]
#[path = "notification_controller/tests.rs"]
mod tests;
