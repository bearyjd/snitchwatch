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
use std::time::{Duration, Instant};

use crate::make_rule::{Ending, Finished, MakeRuleWait, NOT_SENT, NO_ANSWER_AFTER, SENDING};
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
        type MakeRuleController = super::MakeRuleControllerRust;

        /// A request about row `row_id` ended: `ending` is
        /// `make_rule::Ending` (0 not created, 1 created, 2 unknown). No
        /// text: the reasons are bridge text and stay in the sheet's
        /// plain-text result, and `MakeRuleOutcomes.qml` turns this into one
        /// of three fixed notices when that row isn't on screen (PR #111
        /// review, H1).
        #[qsignal]
        fn finished(self: Pin<&mut MakeRuleController>, row_id: QString, ending: i32);

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

        /// TEST ONLY: wait `ms` milliseconds for a result instead of
        /// `make_rule::NO_ANSWER_AFTER`, so a headless probe can see a
        /// silence end the wait. No shipped QML calls it (a guard in
        /// `honest_ui_qml_guards.rs`), so the app always waits the full
        /// deadline (PR #111 review, L4).
        #[qinvokable]
        #[cxx_name = "shortenDeadlineForTests"]
        fn shorten_deadline_for_tests(self: Pin<&mut MakeRuleController>, ms: i32);

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
    /// [`NO_ANSWER_AFTER`], except in the probes.
    no_answer_after: Duration,
    wait: MakeRuleWait,
}

impl Default for MakeRuleControllerRust {
    fn default() -> Self {
        Self {
            busy: false,
            status_text: QString::default(),
            row_id: QString::default(),
            created: false,
            no_answer_after: NO_ANSWER_AFTER,
            wait: MakeRuleWait::default(),
        }
    }
}

impl qobject::MakeRuleController {
    fn begin(mut self: Pin<&mut Self>, row_id: &QString) -> QString {
        if self.busy {
            return QString::from("");
        }
        let request_id = crate::rule_commands::next_request_id("make");
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
        let after = self.no_answer_after;
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
        if let Some((row_id, done, ending)) = gave_up {
            self.finish(row_id, done, ending);
        }
    }

    fn shorten_deadline_for_tests(mut self: Pin<&mut Self>, ms: i32) {
        self.as_mut().rust_mut().no_answer_after =
            Duration::from_millis(u64::try_from(ms).unwrap_or(0));
    }

    /// The wait ended: say how, for the row it was about.
    fn finish(mut self: Pin<&mut Self>, row_id: String, done: Finished, ending: Ending) {
        self.as_mut().set_row_id(QString::from(&row_id));
        self.as_mut().set_created(done.saved);
        self.as_mut().set_busy(false);
        self.as_mut().set_status_text(QString::from(&done.status));
        self.finished(QString::from(&row_id), ending as i32);
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(message) => self.on_message(message),
            Err(error) => tracing::warn!(%error, "MakeRuleController: bad ServerMessage JSON"),
        }
    }

    fn on_message(mut self: Pin<&mut Self>, message: ServerMessage) {
        let done = self.as_mut().rust_mut().wait.on_message(&message);
        if let Some((row_id, done, ending)) = done {
            self.finish(row_id, done, ending);
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        crate::result_feed::spawn_result_feed(
            self.qt_thread(),
            "MakeRuleController",
            Self::on_message,
        );
    }
}
