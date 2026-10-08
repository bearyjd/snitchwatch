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
//! **Cooldown.** [`crate::notifier::CooldownGate`] is ported unchanged.
//!
//! **5-second pending grace period.** Per the original design spec's
//! "Pending decision" notification rule ("only if the Snitchwatch window is
//! hidden AND the row has been pending for more than 5 seconds"): a
//! `Notice::Pending` is not dispatched immediately. A 5-second delay timer is
//! started instead; if the main window is still not active when it fires,
//! *then* the cooldown-gated notification goes out with a "Review" action.
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

use crate::bridge_runtime::{BridgeHandles, BridgeNotice, ReceivedNotice};
use crate::notifier::CooldownGate;
use crate::pending_notice::{show_and_answer, show_plain, PendingTarget};
use snitchwatch_bridge::translator::process_binding::RuleRefusal;

/// Per the design spec: a pending row is only worth a fallback desktop
/// notification once it has been waiting this long with the window hidden.
const PENDING_GRACE_PERIOD: Duration = Duration::from_secs(5);

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
}

impl Default for NotificationControllerRust {
    fn default() -> Self {
        Self {
            window_active: true,
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
/// shown only while its row still waits in its session (Part B).
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
    if !matches!(notice, BridgeNotice::Pending { .. }) {
        let _ = qt_thread.queue(move |qobject| {
            if handles.is_current_session(connection_id) {
                qobject.maybe_dispatch(notice, gate, None);
            }
        });
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(PENDING_GRACE_PERIOD).await;
        let _ = qt_thread.queue(move |qobject| {
            if let Some(target) = PendingTarget::of(&handles, connection_id, &notice) {
                qobject.maybe_dispatch(notice, gate, Some(target));
            }
        });
    });
}

impl qobject::NotificationController {
    /// Runs on the Qt thread (queued from the feed task above). Applies the
    /// window-hidden gate (Pending only) and the cooldown gate, then hands
    /// off to [`Self::dispatch`] if both allow it.
    fn maybe_dispatch(
        mut self: Pin<&mut Self>,
        notice: BridgeNotice,
        gate: Arc<Mutex<CooldownGate>>,
        target: Option<PendingTarget>,
    ) {
        if matches!(notice, BridgeNotice::Pending { .. }) && *self.window_active() {
            // Window came back to the front during the grace period — the
            // in-app pending-count handler (Task 7) already surfaced this,
            // no fallback notification needed.
            return;
        }
        let allow = gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .should_fire(&notice, Instant::now());
        if !allow {
            return;
        }
        match target {
            Some(target) => self.as_mut().dispatch_pending(target),
            None => dispatch(notice),
        }
    }

    /// The pending notice, on the bridge runtime (`crate::pending_notice`).
    /// "Review" raises the window through `reviewRequested`.
    fn dispatch_pending(self: Pin<&mut Self>, target: PendingTarget) {
        let qt_thread = self.qt_thread();
        let runtime = target.handles().runtime().clone();
        runtime.spawn(async move {
            let conn = match zbus::Connection::session().await {
                Ok(conn) => conn,
                Err(error) => {
                    tracing::warn!(%error, "no session bus for the pending notification");
                    return;
                }
            };
            show_and_answer(&conn, target, move || {
                let _ = qt_thread.queue(|qobject| qobject.review_requested());
            })
            .await;
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
mod tests {
    use super::*;

    /// Issue #44: the fixed explanation, with no "Review" action.
    #[test]
    fn verdict_not_remembered_is_explained() {
        let notice = BridgeNotice::VerdictNotRemembered { row_id: 3 };
        let (summary, body) = notice_text(&notice).unwrap();
        assert_eq!(summary, "Snitchwatch — answer not remembered");
        assert_eq!(body, RuleRefusal::ProcessFileUnknown.describe());
    }

    /// Prompt-slot plan, part A: the release summary, fixed text around
    /// the count, with no "Review" (the prompt is already answered).
    #[test]
    fn a_prompt_slot_summary_is_fixed_text() {
        let notice = BridgeNotice::PromptSlotSummary {
            row_id: 3,
            count: 2,
        };
        let (summary, body) = notice_text(&notice).unwrap();
        assert_eq!(summary, "Snitchwatch — while a prompt was open");
        assert_eq!(
            body,
            "While that prompt was open, the firewall applied its default action 2 times \
             (retries count again)."
        );
    }

    /// A Pending notice never goes out as plain text: only through
    /// `pending_notice`, with its actions heard from the server alone.
    #[test]
    fn a_pending_notice_has_no_plain_text_form() {
        let notice = BridgeNotice::Pending {
            row_id: 3,
            process: "curl".into(),
        };
        assert_eq!(notice_text(&notice), None);
    }
}
