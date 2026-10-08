//! `PromptSlotStatus` — who holds opensnitchd's single prompt slot, for the
//! window banner (`PromptSlotBanner.qml`). Plan
//! `docs/superpowers/plans/2026-10-08-prompt-slot-ux.md`, part A, with the
//! honesty half of issue #78 (`crate::prompt_slot_text`).
//!
//! Fed from the client runtime's `PromptSlot` and filter-pause watch channels,
//! each change applied only while it still belongs to the live session (like
//! `TrayController`). `supported` is the session's `promptSlot` capability:
//! without it `main.qml` keeps its own estimate from pending rows.

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;

use tokio::sync::watch;

use crate::bridge_runtime::{BridgeHandles, ReceivedPauseState, ReceivedPromptSlot};
use crate::prompt_slot_text::{banner_text, SlotView};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        /// Whether the live session's bridge sends `PromptSlot` messages.
        #[qproperty(bool, supported)]
        /// Whether a prompt holds the slot in the live session.
        #[qproperty(bool, held)]
        /// The holder's session-qualified row id (as `ConnectionsModel`
        /// stores it), or empty.
        #[qproperty(QString, row_id, cxx_name = "rowId")]
        /// The banner text (`prompt_slot_text::banner_text`). Carries the
        /// program and host: show it as plain text only.
        #[qproperty(QString, text)]
        type PromptSlotStatus = super::PromptSlotStatusRust;

        /// Start the live feed. No-op when the bridge isn't running. Called
        /// from `main.qml`'s `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut PromptSlotStatus>);

        /// Recompute the properties (the wait time moves on by itself; a QML
        /// timer calls this while a prompt is held).
        #[qinvokable]
        fn refresh(self: Pin<&mut PromptSlotStatus>);
    }

    impl cxx_qt::Threading for PromptSlotStatus {}
}

/// Rust-side state for [`qobject::PromptSlotStatus`].
#[derive(Default)]
pub struct PromptSlotStatusRust {
    supported: bool,
    held: bool,
    row_id: QString,
    text: QString,
    slot: ReceivedPromptSlot,
    paused: bool,
}

impl qobject::PromptSlotStatus {
    fn start_bridge_feed(self: Pin<&mut Self>) {
        let (Some(slot_rx), Some(pause_rx), Some(handles)) = (
            crate::bridge_runtime::prompt_slot_rx(),
            crate::bridge_runtime::pause_rx(),
            crate::bridge_runtime::handles(),
        ) else {
            tracing::warn!("PromptSlotStatus: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let initial_slot = slot_rx.borrow().clone();
        let initial_pause = pause_rx.borrow().clone();
        let initial_handles = handles.clone();
        let _ = qt_thread.queue(move |mut qobject| {
            if initial_handles.is_current_session(initial_slot.connection_id) {
                qobject.as_mut().rust_mut().slot = initial_slot;
            }
            if initial_handles.is_current_session(initial_pause.connection_id) {
                qobject.as_mut().rust_mut().paused = initial_pause.state.paused;
            }
            qobject.refresh();
        });

        spawn_slot_feed(&handles, qt_thread.clone(), slot_rx);
        spawn_pause_feed(&handles, qt_thread, pause_rx);
    }

    fn refresh(mut self: Pin<&mut Self>) {
        let handles = crate::bridge_runtime::handles();
        let supported = handles
            .as_ref()
            .is_some_and(|handles| handles.advertises_prompt_slot());
        let current = handles
            .as_ref()
            .is_some_and(|handles| handles.is_current_session(self.rust().slot.connection_id));
        let shown = shown_holder(&self.rust().slot, current, now_ms(), self.rust().paused);
        let (row_id, text) = shown.clone().unwrap_or_default();
        self.as_mut().set_supported(supported);
        self.as_mut().set_held(shown.is_some());
        self.as_mut().set_row_id(QString::from(&row_id));
        self.as_mut().set_text(QString::from(&text));
    }
}

type StatusThread = cxx_qt::CxxQtThread<qobject::PromptSlotStatus>;

/// Each change applies only if it still belongs to the live session when the
/// queued Qt callback runs.
fn spawn_slot_feed(
    handles: &BridgeHandles,
    qt_thread: StatusThread,
    mut slot_rx: watch::Receiver<ReceivedPromptSlot>,
) {
    let handles = handles.clone();
    handles.runtime().clone().spawn(async move {
        while slot_rx.changed().await.is_ok() {
            let received = slot_rx.borrow().clone();
            let session_handles = handles.clone();
            let _ = qt_thread.queue(move |mut qobject| {
                if session_handles.is_current_session(received.connection_id) {
                    qobject.as_mut().rust_mut().slot = received;
                    qobject.refresh();
                }
            });
        }
    });
}

fn spawn_pause_feed(
    handles: &BridgeHandles,
    qt_thread: StatusThread,
    mut pause_rx: watch::Receiver<ReceivedPauseState>,
) {
    let handles = handles.clone();
    handles.runtime().clone().spawn(async move {
        while pause_rx.changed().await.is_ok() {
            let received = pause_rx.borrow().clone();
            let session_handles = handles.clone();
            let _ = qt_thread.queue(move |mut qobject| {
                if session_handles.is_current_session(received.connection_id) {
                    qobject.as_mut().rust_mut().paused = received.state.paused;
                    qobject.refresh();
                }
            });
        }
    });
}

/// The holder's session-qualified row id and banner text, or `None` when the
/// slot is free or the state belongs to a session that is no longer live.
pub(crate) fn shown_holder(
    slot: &ReceivedPromptSlot,
    current_session: bool,
    now_ms: u64,
    paused: bool,
) -> Option<(String, String)> {
    if !current_session {
        return None;
    }
    let holder = slot.holder.as_ref()?;
    let text = banner_text(&SlotView {
        what: &holder.what,
        waited_secs: now_ms.saturating_sub(holder.since_ms) / 1000,
        holders: slot.holders,
        defaulted_at_least: slot.defaulted_at_least,
        paused,
    });
    Some((format!("{}:{}", slot.connection_id, holder.row_id), text))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::prompt_slot::PromptSlotHolder;

    fn slot(holder: bool) -> ReceivedPromptSlot {
        ReceivedPromptSlot {
            connection_id: 3,
            holder: holder.then(|| PromptSlotHolder {
                row_id: "ask-7".into(),
                what: "steam → example.com".into(),
                since_ms: 10_000,
            }),
            holders: u32::from(holder),
            defaulted_at_least: Some(2),
        }
    }

    #[test]
    fn the_holder_is_shown_with_a_session_qualified_row_id() {
        let (row_id, text) = shown_holder(&slot(true), true, 25_500, false).unwrap();
        assert_eq!(row_id, "3:ask-7");
        assert_eq!(
            text,
            banner_text(&SlotView {
                what: "steam → example.com",
                waited_secs: 15,
                holders: 1,
                defaulted_at_least: Some(2),
                paused: false,
            })
        );
    }

    #[test]
    fn a_pause_changes_the_text() {
        let (_, text) = shown_holder(&slot(true), true, 25_500, true).unwrap();
        assert!(
            text.contains(&crate::prompt_slot_text::paused_while_waiting(1)),
            "{text}"
        );
    }

    #[test]
    fn a_free_slot_or_an_old_session_shows_nothing() {
        assert_eq!(shown_holder(&slot(false), true, 25_500, false), None);
        assert_eq!(shown_holder(&slot(true), false, 25_500, false), None);
    }
}
