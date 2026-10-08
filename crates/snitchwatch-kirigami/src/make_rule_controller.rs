//! `MakeRuleController`: the outcome of "Make a rule…" for
//! `MakeRuleSheet.qml` (PR #108 security review, M1).
//!
//! The sheet asks [`begin`](qobject::MakeRuleController::begin) for a
//! request id, then sends the rule through `ConnectionsModel.makeRule` with
//! it. The bridge answers one `RuleCommandResult` per request id, to this
//! connection only, and this controller says "The rule was created." only
//! when that result is Ok; otherwise the refusal's reason, or that the
//! outcome is unknown after `make_rule::NO_ANSWER_AFTER`. The Qt-free parts
//! live in [`crate::make_rule`]. Every string it exposes is plain text.

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::make_rule::{Finished, MakeRuleWait, NOT_SENT, NO_ANSWER_AFTER, SENDING};
use snitchwatch_bridge::ws_messages::ServerMessage;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// "Make a rule…" request state for `MakeRuleSheet.qml`.
        #[qobject]
        #[qml_element]
        /// Waiting for the bridge's result; the sheet sends nothing more.
        #[qproperty(bool, busy)]
        /// The last request's progress or outcome, as plain text.
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        /// The row the status is about.
        #[qproperty(QString, row_id, cxx_name = "rowId")]
        /// Whether the last request's result was Ok.
        #[qproperty(bool, created)]
        /// How long to wait for the result, in milliseconds
        /// (`make_rule::NO_ANSWER_AFTER`). Only the headless probes change
        /// it, to see a silence end the wait.
        #[qproperty(i32, no_answer_after_ms, cxx_name = "noAnswerAfterMs")]
        type MakeRuleController = super::MakeRuleControllerRust;

        /// A request ended, for row `row_id`, saying `status`. The sheet
        /// shows it as a notice when that row isn't on screen.
        #[qsignal]
        fn finished(self: Pin<&mut MakeRuleController>, row_id: QString, status: QString);

        /// Feed the bridge's `RuleCommandResult`s to this controller.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut MakeRuleController>);

        /// Start waiting for a request about `row_id` and return its id, to
        /// pass to `ConnectionsModel.makeRule`. Empty while one is waiting.
        #[qinvokable]
        fn begin(self: Pin<&mut MakeRuleController>, row_id: &QString) -> QString;

        /// The request `begin` started wasn't sent, because of `reason`
        /// (`ConnectionsModel.makeRule`'s answer; empty: the generic text).
        #[qinvokable]
        #[cxx_name = "notSent"]
        fn not_sent(self: Pin<&mut MakeRuleController>, reason: &QString);

        /// Give up waiting after a silence (called by a one-second QML timer).
        #[qinvokable]
        fn poll(self: Pin<&mut MakeRuleController>);

        /// One bridge message as JSON (the headless probes' feed).
        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut MakeRuleController>, json: &QString);
    }

    impl cxx_qt::Threading for MakeRuleController {}
}

/// Rust-side state for [`qobject::MakeRuleController`].
pub struct MakeRuleControllerRust {
    busy: bool,
    status_text: QString,
    row_id: QString,
    created: bool,
    no_answer_after_ms: i32,
    wait: MakeRuleWait,
}

impl Default for MakeRuleControllerRust {
    fn default() -> Self {
        Self {
            busy: false,
            status_text: QString::default(),
            row_id: QString::default(),
            created: false,
            no_answer_after_ms: i32::try_from(NO_ANSWER_AFTER.as_millis()).unwrap_or(i32::MAX),
            wait: MakeRuleWait::default(),
        }
    }
}

fn next_request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "make-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Only results, the same filter as the rule editor's feed.
fn interests_make_rule(message: &ServerMessage) -> bool {
    matches!(message, ServerMessage::RuleCommandResult { .. })
}

impl qobject::MakeRuleController {
    fn begin(mut self: Pin<&mut Self>, row_id: &QString) -> QString {
        if self.busy {
            return QString::from("");
        }
        let request_id = next_request_id();
        self.as_mut()
            .rust_mut()
            .wait
            .begin(request_id.clone(), row_id.to_string(), Instant::now());
        self.as_mut().set_row_id(row_id.clone());
        self.as_mut().set_created(false);
        self.as_mut().set_busy(true);
        self.set_status_text(QString::from(SENDING));
        QString::from(&request_id)
    }

    fn not_sent(mut self: Pin<&mut Self>, reason: &QString) {
        self.as_mut().rust_mut().wait.abandon();
        self.as_mut().set_created(false);
        self.as_mut().set_busy(false);
        let status = if reason.is_empty() {
            QString::from(NOT_SENT)
        } else {
            reason.clone()
        };
        self.set_status_text(status);
    }

    fn poll(mut self: Pin<&mut Self>) {
        let after = Duration::from_millis(u64::try_from(self.no_answer_after_ms).unwrap_or(0));
        let handles = crate::bridge_runtime::handles();
        let is_current = |session| {
            handles
                .as_ref()
                .is_some_and(|h| h.is_current_session(session))
        };
        let gave_up = self
            .as_mut()
            .rust_mut()
            .wait
            .poll(Instant::now(), after, is_current);
        if let Some((row_id, done)) = gave_up {
            self.finish(row_id, done);
        }
    }

    /// The wait ended: say how, for the row it was about.
    fn finish(mut self: Pin<&mut Self>, row_id: String, done: Finished) {
        self.as_mut().set_row_id(QString::from(&row_id));
        self.as_mut().set_created(done.created);
        self.as_mut().set_busy(false);
        self.as_mut().set_status_text(QString::from(&done.status));
        self.finished(QString::from(&row_id), QString::from(&done.status));
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(message) => self.on_message(message),
            Err(error) => tracing::warn!(%error, "MakeRuleController: bad ServerMessage JSON"),
        }
    }

    fn on_message(mut self: Pin<&mut Self>, message: ServerMessage) {
        let done = self.as_mut().rust_mut().wait.on_message(&message);
        if let Some((row_id, done)) = done {
            self.finish(row_id, done);
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("MakeRuleController: bridge not running; no rule can be made");
            return;
        };
        let qt_thread = self.qt_thread();
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "MakeRuleController",
            interests_make_rule,
            move |connection_id, message, _json| {
                let session_handles = session_handles.clone();
                let message = message.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if session_handles.is_current_session(connection_id) {
                        qobject.on_message(message);
                    }
                });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_valid_and_distinct() {
        let (a, b) = (next_request_id(), next_request_id());
        assert_ne!(a, b);
        for id in [&a, &b] {
            assert!(
                snitchwatch_bridge::ws_messages::valid_request_id(id),
                "{id}"
            );
        }
    }

    #[test]
    fn the_feed_takes_only_results() {
        assert!(interests_make_rule(&ServerMessage::RuleCommandResult {
            request_id: "make-1".into(),
            outcome: snitchwatch_bridge::ws_messages::RuleCommandOutcome::Ok,
        }));
        assert!(!interests_make_rule(&ServerMessage::ClearConnectionRows));
    }
}
