//! `TrayController` — system tray state feed (Task 18).
//!
//! **Verified before building:** `qmake6 -query QT_INSTALL_QML` resolves to
//! `/usr/lib64/qt6/qml` in this environment, and
//! `Qt/labs/platform/liblabsplatformplugin.so` exists there — the
//! `Qt.labs.platform.SystemTrayIcon` primary path from the plan is available,
//! so no `KStatusNotifierItem`/`cxx-kde-frameworks` fallback work was needed.
//! The tray icon/menu itself is declared directly in `main.qml`; this
//! `QObject` only feeds it the tooltip/menu-label strings derived from the
//! bridge's `TrayState` and `FilterPauseState` by [`crate::tray`]'s pure,
//! unit-tested functions, plus Q04's firewall and process-monitor lines from
//! [`crate::tray_health`] (the bridge's `DiagnosticsReport`).

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QDateTime, QString};

use tokio::sync::{oneshot, watch};

use crate::bridge_runtime::{
    BridgeHandles, BridgePauseState, ReceivedPauseState, ReceivedPromptSlot, ReceivedTrayState,
};
use crate::tray::{
    build_set_filtering_paused_json, derive_menu_label, derive_tooltip_with_slot,
    live_slot_holders, menu_label_token, startup_tooltip,
};
use crate::tray_health::{compose_tooltip, firewall_report};
use crate::tray_session::{cleanup_job, watch_sessions, TraySession};
use snitchwatch_bridge::ws_messages::ServerMessage;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Tray icon state feed, bound by the `Qt.labs.platform.SystemTrayIcon`
        /// declared in `main.qml`.
        #[qobject]
        #[qml_element]
        /// Tooltip text for the current tray and pause state (`derive_tooltip`).
        #[qproperty(QString, tooltip)]
        /// One of `default` / `pause_filtering` / `resume_filtering` /
        /// `reconnect` (`derive_menu_label`'s token, see
        /// `crate::tray::menu_label_token`), so the tray's context-menu items
        /// can react to daemon/filter state without duplicating the
        /// derivation logic in QML.
        #[qproperty(QString, menu_label, cxx_name = "menuLabel")]
        /// Local end time ("HH:MM") of the active filtering pause, or empty
        /// when not paused or unknown (issue #47).
        #[qproperty(QString, paused_until, cxx_name = "pausedUntil")]
        /// Whether the live session's bridge advertised
        /// `pauseAnswersWaiting`: a pause also lets the prompts already
        /// waiting through once (issue #78). The pause items promise that
        /// only while this is true; false until a bridge says so.
        #[qproperty(bool, pause_answers_waiting, cxx_name = "pauseAnswersWaiting")]
        type TrayController = super::TrayControllerRust;

        /// Start the live feed: subscribe to the bridge's tray-state and
        /// pause-state watch channels and update the properties on every
        /// change. No-op when the bridge isn't running. Called from
        /// `main.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut TrayController>);

        /// Emitted with the JSON-encoded `ClientMessage::SetFilteringPaused`
        /// for the tray menu's pause/resume items. `main.qml` connects this
        /// to `bridgeFeed.sendClientJson` — the same in-process path
        /// `BridgeFeed::submitVerdict` uses.
        #[qsignal]
        #[cxx_name = "filteringToggleRequested"]
        fn filtering_toggle_requested(self: Pin<&mut TrayController>, json: QString);

        /// Pause filtering for `secs` seconds (bridge auto-allows every
        /// `AskRule` until then). The tray offers 5 min, 30 min and 1 hour;
        /// the bridge rejects any other length (issue #47).
        #[qinvokable]
        #[cxx_name = "pauseFor"]
        fn pause_for(self: Pin<&mut TrayController>, secs: i32);

        /// End the pause now and resume normal prompting.
        #[qinvokable]
        fn resume(self: Pin<&mut TrayController>);
    }

    impl cxx_qt::Threading for TrayController {}
}

/// Rust-side state for [`qobject::TrayController`].
pub struct TrayControllerRust {
    tooltip: QString,
    menu_label: QString,
    paused_until: QString,
    pause_answers_waiting: bool,
    /// Tray, pause and firewall state, each tagged with its session. Only
    /// the live session's is shown, so no default claims a connection (Q04).
    session: TraySession,
    /// Dropped with the controller, which ends its session watch.
    _session_watch_stop: Option<oneshot::Sender<()>>,
    /// How many prompts hold the daemon's single slot (issue #78;
    /// `PromptSlot` messages), as of session `slot_session`.
    slot_holders: u32,
    slot_session: u64,
}

impl Default for TrayControllerRust {
    fn default() -> Self {
        Self {
            tooltip: QString::from(&startup_tooltip()),
            menu_label: QString::from("default"),
            paused_until: QString::default(),
            pause_answers_waiting: false,
            session: TraySession::default(),
            _session_watch_stop: None,
            slot_holders: 0,
            slot_session: 0,
        }
    }
}

impl qobject::TrayController {
    fn start_bridge_feed(mut self: Pin<&mut Self>) {
        let (Some(tray_rx), Some(pause_rx), Some(handles)) = (
            crate::bridge_runtime::tray_rx(),
            crate::bridge_runtime::pause_rx(),
            crate::bridge_runtime::handles(),
        ) else {
            tracing::warn!("TrayController: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();

        // Apply the current (initial) state immediately, then react to every
        // subsequent change — mirrors the Tauri shell's `Tray::install`,
        // which rendered `TrayState::Idle` up front before its watch loop.
        let initial_tray = tray_rx.borrow().clone();
        let initial_pause = pause_rx.borrow().clone();
        let initial_handles = handles.clone();
        let _ = qt_thread.queue(move |mut qobject| {
            if initial_handles.is_current_session(initial_tray.connection_id) {
                let next = qobject
                    .rust()
                    .session
                    .with_tray(initial_tray.connection_id, initial_tray.state);
                qobject.as_mut().rust_mut().session = next;
            }
            if initial_handles.is_current_session(initial_pause.connection_id) {
                let next = qobject
                    .rust()
                    .session
                    .with_pause(initial_pause.connection_id, initial_pause.state);
                qobject.as_mut().rust_mut().session = next;
            }
            qobject.refresh();
        });

        // Each change is applied only if it still belongs to the live bridge
        // session when the queued Qt callback runs.
        if let Some(slot_rx) = crate::bridge_runtime::prompt_slot_rx() {
            spawn_slot_feed(&handles, qt_thread.clone(), slot_rx);
        }
        spawn_tray_feed(&handles, qt_thread.clone(), tray_rx);
        spawn_pause_feed(&handles, qt_thread.clone(), pause_rx);
        spawn_diagnostics_feed(&handles, qt_thread.clone());
        let stop = spawn_session_watch(&handles, qt_thread);
        self.as_mut().rust_mut()._session_watch_stop = Some(stop);
    }
}

type TrayThread = cxx_qt::CxxQtThread<qobject::TrayController>;

/// The prompt slot's holders (issue #78), from the current value on.
fn spawn_slot_feed(
    handles: &BridgeHandles,
    qt_thread: TrayThread,
    mut slot_rx: watch::Receiver<ReceivedPromptSlot>,
) {
    let handles = handles.clone();
    handles.runtime().clone().spawn(async move {
        loop {
            let received = slot_rx.borrow_and_update().clone();
            let session_handles = handles.clone();
            let _ = qt_thread.queue(move |mut qobject| {
                if session_handles.is_current_session(received.connection_id) {
                    qobject.as_mut().rust_mut().slot_holders = received.holders;
                    qobject.as_mut().rust_mut().slot_session = received.connection_id;
                    qobject.refresh();
                }
            });
            if slot_rx.changed().await.is_err() {
                break;
            }
        }
    });
}

fn spawn_tray_feed(
    handles: &BridgeHandles,
    qt_thread: TrayThread,
    mut tray_rx: watch::Receiver<ReceivedTrayState>,
) {
    let handles = handles.clone();
    handles.runtime().clone().spawn(async move {
        while tray_rx.changed().await.is_ok() {
            let received = tray_rx.borrow().clone();
            let session_handles = handles.clone();
            let _ = qt_thread.queue(move |mut qobject| {
                if session_handles.is_current_session(received.connection_id) {
                    let next = qobject
                        .rust()
                        .session
                        .with_tray(received.connection_id, received.state);
                    qobject.as_mut().rust_mut().session = next;
                    qobject.refresh();
                }
            });
        }
    });
}

fn spawn_pause_feed(
    handles: &BridgeHandles,
    qt_thread: TrayThread,
    mut pause_rx: watch::Receiver<ReceivedPauseState>,
) {
    let handles = handles.clone();
    handles.runtime().clone().spawn(async move {
        while pause_rx.changed().await.is_ok() {
            let received = pause_rx.borrow().clone();
            let session_handles = handles.clone();
            let _ = qt_thread.queue(move |mut qobject| {
                if session_handles.is_current_session(received.connection_id) {
                    let next = qobject
                        .rust()
                        .session
                        .with_pause(received.connection_id, received.state);
                    qobject.as_mut().rust_mut().session = next;
                    qobject.refresh();
                }
            });
        }
    });
}

/// The firewall check from every `DiagnosticsReport`, starting with the one
/// in the snapshot `spawn_feed` requests. Never `RecheckDiagnostics`: it
/// clears the bridge's stored daemon alerts before reporting.
fn spawn_diagnostics_feed(handles: &BridgeHandles, qt_thread: TrayThread) {
    let session_handles = handles.clone();
    crate::bridge_dispatch::spawn_feed(
        handles,
        "TrayController",
        crate::bridge_dispatch::interests_diagnostics,
        move |connection_id, msg, _json| {
            let ServerMessage::DiagnosticsReport { checks } = msg else {
                return;
            };
            let report = firewall_report(checks);
            let session_handles = session_handles.clone();
            let _ = qt_thread.queue(move |mut qobject| {
                if !session_handles.is_current_session(connection_id) {
                    return;
                }
                let session = &qobject.rust().session;
                let seen = session
                    .firewall
                    .observe(connection_id, report, local_now_hh_mm);
                let next = session.with_firewall(seen);
                qobject.as_mut().rust_mut().session = next;
                qobject.refresh();
            });
        },
    );
}

/// How often the tray checks for a disconnect or a new session. The link has
/// no change notification; `BridgeFeed`'s banner polls it every second.
const SESSION_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Forget what a session that's no longer live reported, on the first pass
/// and on every disconnect or session change, so the tray never shows it as
/// current (Q04). Each cleanup reads the live session on the Qt thread
/// (`tray_session::cleanup_job`). Ends when the returned sender is dropped.
fn spawn_session_watch(handles: &BridgeHandles, qt_thread: TrayThread) -> oneshot::Sender<()> {
    let (stop_tx, stop_rx) = oneshot::channel();
    let watch_handles = handles.clone();
    let job_handles = handles.clone();
    handles.runtime().spawn(watch_sessions(
        move || watch_handles.live_session(),
        move || {
            let job_handles = job_handles.clone();
            let job = cleanup_job(move || job_handles.live_session());
            qt_thread
                .queue(move |mut qobject| {
                    let next = job(&qobject.rust().session);
                    qobject.as_mut().rust_mut().session = next;
                    qobject.refresh();
                })
                .is_ok()
        },
        SESSION_POLL,
        stop_rx,
    ));
    stop_tx
}

impl qobject::TrayController {
    fn pause_for(self: Pin<&mut Self>, secs: i32) {
        let Ok(secs) = u64::try_from(secs) else {
            tracing::warn!(secs, "TrayController: ignoring a negative pause length");
            return;
        };
        let json = build_set_filtering_paused_json(true, Some(secs));
        self.filtering_toggle_requested(QString::from(&json));
    }

    fn resume(self: Pin<&mut Self>) {
        let json = build_set_filtering_paused_json(false, None);
        self.filtering_toggle_requested(QString::from(&json));
    }
}

impl qobject::TrayController {
    fn refresh(mut self: Pin<&mut Self>) {
        let handles = crate::bridge_runtime::handles();
        let live = |session: Option<u64>| {
            session.is_some_and(|id| handles.as_ref().is_some_and(|h| h.is_current_session(id)))
        };
        let session = self.rust().session.clone();
        let Some(tray) = session.tray.filter(|t| live(Some(t.session))) else {
            // Bridge unavailable, or its state not here yet: wait, offer nothing.
            self.as_mut().set_tooltip(QString::from(&startup_tooltip()));
            self.as_mut().set_menu_label(QString::from("default"));
            self.as_mut().set_paused_until(QString::default());
            self.as_mut().set_pause_answers_waiting(false);
            return;
        };
        let pause = session
            .pause
            .filter(|p| live(Some(p.session)))
            .map_or(BridgePauseState::NOT_PAUSED, |p| p.value);
        let until = match (pause.paused, pause.expires_at_unix_ms) {
            (true, Some(ms)) => local_hh_mm(ms),
            _ => String::new(),
        };
        let session_is_current = handles
            .as_ref()
            .is_some_and(|handles| handles.is_current_session(self.rust().slot_session));
        let answers_waiting = handles
            .as_ref()
            .is_some_and(|handles| handles.advertises_pause_answers_waiting());
        let holders = live_slot_holders(self.rust().slot_holders, session_is_current);
        let state_text = derive_tooltip_with_slot(&tray.value, &pause, &until, holders);
        let firewall = live(Some(session.firewall.session()))
            .then(|| session.firewall.line())
            .flatten();
        let tooltip = compose_tooltip(&state_text, firewall.as_deref());
        let label = derive_menu_label(&tray.value, &pause);
        self.as_mut().set_tooltip(QString::from(&tooltip));
        self.as_mut()
            .set_menu_label(QString::from(menu_label_token(&label)));
        self.as_mut().set_paused_until(QString::from(&until));
        self.as_mut().set_pause_answers_waiting(answers_waiting);
    }
}

/// `unix_ms` as a local "HH:MM" time. Qt's local time (`TZ`, else
/// `/etc/localtime`), as `connections_model::verdict_windows` reads it: not
/// `QTimeZone::system_time_zone()`, which fell back to UTC in a sandbox whose
/// `/etc/localtime` is a plain file.
fn local_hh_mm(unix_ms: u64) -> String {
    let mut local = QDateTime::current_date_time();
    local.set_msecs_since_epoch(i64::try_from(unix_ms).unwrap_or(i64::MAX));
    local.time().format(&QString::from("HH:mm")).to_string()
}

/// Now, as a local "HH:MM" time.
fn local_now_hh_mm() -> String {
    QDateTime::current_date_time()
        .time()
        .format(&QString::from("HH:mm"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Paused until" and the firewall line's "noticed" time read the same
    /// local clock (Q04), so one tooltip never mixes UTC and local time.
    #[test]
    fn pause_end_and_noticed_times_share_the_local_clock() {
        let agree = (0..3).any(|_| {
            let now_ms = u64::try_from(QDateTime::current_msecs_since_epoch()).unwrap();
            local_hh_mm(now_ms) == local_now_hh_mm()
        });
        assert!(
            agree,
            "{} vs {}",
            local_hh_mm(u64::try_from(QDateTime::current_msecs_since_epoch()).unwrap()),
            local_now_hh_mm()
        );
    }
}
