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
//!
//! **"Review" action → raise window.** The action button, when clicked, is
//! observed on a scratch thread (`NotificationHandle::wait_for_action`
//! blocks) and queued back as the `reviewRequested` signal, which
//! `main.qml` connects to the same raise/`requestActivate()` call Task 7's
//! pending-count handler uses.

use core::pin::Pin;
use cxx_qt::Threading;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::bridge_runtime::BridgeNotice;
use crate::notifier::CooldownGate;

/// D-Bus action id for the "Review" button; matched against
/// `NotificationHandle::wait_for_action`'s callback argument.
const REVIEW_ACTION_ID: &str = "review";

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
        let Some(mut notice_rx) = crate::bridge_runtime::notice_rx() else {
            tracing::warn!("NotificationController: bridge not running; live feed disabled");
            return;
        };
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("NotificationController: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        // Shared across every notice this feed ever sees (including the
        // delayed Pending checks below), so the 30s-default cooldown is
        // tracked per `NoticeKey` across the whole feed's lifetime, not reset
        // per notice.
        let gate = Arc::new(Mutex::new(CooldownGate::new()));

        handles.runtime().spawn(async move {
            loop {
                match notice_rx.recv().await {
                    Ok(notice) => {
                        if matches!(notice, BridgeNotice::Pending { .. }) {
                            // Grace period: only actually consider dispatching
                            // once the row has been pending for 5s. Spawned
                            // rather than `sleep`-ing inline so a burst of
                            // Pending notices doesn't stall this loop from
                            // observing DaemonAway/FilterPauseExpired meanwhile.
                            let qt_thread = qt_thread.clone();
                            let gate = gate.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(PENDING_GRACE_PERIOD).await;
                                let _ = qt_thread.queue(move |qobject| {
                                    qobject.maybe_dispatch(notice, gate);
                                });
                            });
                        } else {
                            let gate = gate.clone();
                            let _ = qt_thread.queue(move |qobject| {
                                qobject.maybe_dispatch(notice, gate);
                            });
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            skipped = n,
                            "NotificationController feed lagged behind bridge"
                        )
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

impl qobject::NotificationController {
    /// Runs on the Qt thread (queued from the feed task above). Applies the
    /// window-hidden gate (Pending only) and the cooldown gate, then hands
    /// off to [`Self::dispatch`] if both allow it.
    fn maybe_dispatch(
        mut self: Pin<&mut Self>,
        notice: BridgeNotice,
        gate: Arc<Mutex<CooldownGate>>,
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
        self.as_mut().dispatch(notice);
    }

    /// Build and show the notification on a scratch thread (`notify-rust`'s
    /// `wait_for_action` blocks the calling thread until the notification
    /// closes), queuing `reviewRequested` back if the action fires.
    fn dispatch(self: Pin<&mut Self>, notice: BridgeNotice) {
        let (summary, body, reviewable) = match &notice {
            BridgeNotice::Pending { process, .. } => (
                "Snitchwatch — pending decision",
                format!("{process} is asking to connect"),
                true,
            ),
            BridgeNotice::DaemonAway => (
                "Snitchwatch — daemon unreachable",
                "opensnitchd has been unreachable for 30 seconds.".to_string(),
                false,
            ),
            BridgeNotice::FilterPauseExpired => (
                "Snitchwatch — filtering resumed",
                "Your pause timer expired.".to_string(),
                false,
            ),
        };

        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification
                .summary(summary)
                .body(&body)
                .icon("security-high");
            if reviewable {
                notification.action(REVIEW_ACTION_ID, "Review");
            }
            match notification.show() {
                Ok(handle) => {
                    handle.wait_for_action(|action| {
                        if action == REVIEW_ACTION_ID {
                            let _ = qt_thread.queue(|qobject| {
                                qobject.review_requested();
                            });
                        }
                    });
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to dispatch desktop notification");
                }
            }
        });
    }
}
