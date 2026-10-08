//! `RuleEditorController`: the rule editor for `RuleEditorSheet.qml`
//! (roadmap P2.1).
//!
//! The Qt-free parts live in [`crate::rules::editor`] (the draft, its wire
//! shape and the bridge's own checks) and [`crate::rules::editor_view`]
//! (messages and texts). The bridge is authoritative: it runs the same
//! checks again, refuses read-only rules, and answers one
//! `RuleCommandResult` per request id, to this connection only. This
//! controller acts only on the result it is waiting for, from the live
//! bridge session. Every string it exposes is plain text.

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::Value;
use std::time::Instant;

use crate::rules::editor::{self, RuleDraft};
use crate::rules::editor_profile;
use crate::rules::editor_view;
use crate::rules::simulator::SimulationForm;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

const SAVING: &str = "Saving…";
/// A profile rule is saved, not yet installed: the Profiles page shows
/// whether the firewall has it.
const PROFILE_SAVED: &str =
    "Saved to the profile. Its status below says whether the firewall installed it.";
const BAD_DRAFT: &str = "The rule couldn't be read. Close the editor and open it again.";

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Editor state for `RuleEditorSheet.qml`.
        #[qobject]
        #[qml_element]
        /// Waiting for the bridge's result; Save is disabled.
        #[qproperty(bool, busy)]
        /// The last outcome or refusal, as plain text.
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        /// The name of the rule being edited; empty for a new rule.
        #[qproperty(QString, editing_name, cxx_name = "editingName")]
        /// `rules::editor::EditorCheck` of the last checked draft, as JSON.
        #[qproperty(QString, check_json, cxx_name = "checkJson")]
        /// The profile a new rule is for (issue #46); empty for a firewall
        /// rule.
        #[qproperty(QString, profile_id, cxx_name = "profileId")]
        type RuleEditorController = super::RuleEditorControllerRust;

        /// The bridge confirmed the rule; the sheet closes.
        #[qsignal]
        fn saved(self: Pin<&mut RuleEditorController>);

        /// Listen for the bridge's results. No-op headless.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut RuleEditorController>);

        /// Handle one `ServerMessage` (JSON). The live feed calls this; so do
        /// headless QML probes.
        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut RuleEditorController>, json: &QString);

        /// The operands, their match kinds and help, and the duration
        /// presets, as JSON.
        #[qinvokable]
        #[cxx_name = "catalogueJson"]
        fn catalogue_json(self: &RuleEditorController) -> QString;

        /// Start a new rule; returns its draft as JSON.
        #[qinvokable]
        #[cxx_name = "newRule"]
        fn new_rule(self: Pin<&mut RuleEditorController>) -> QString;

        /// Start a new rule for profile `profile_id` (issue #46); returns its
        /// draft as JSON. Saving sends `AddProfileRule`.
        #[qinvokable]
        #[cxx_name = "newProfileRule"]
        fn new_profile_rule(self: Pin<&mut RuleEditorController>, profile_id: &QString) -> QString;

        /// Start a new rule for a connection (`SimulationForm` JSON, as
        /// `ConnectionsModel.simulationPrefillJson` gives); returns its
        /// draft as JSON.
        #[qinvokable]
        #[cxx_name = "prefill"]
        fn prefill(self: Pin<&mut RuleEditorController>, form_json: &QString) -> QString;

        /// Edit a cached rule (`RulesModel.editableRuleJson`); returns its
        /// draft as JSON, or empty with the reason in `statusText`.
        #[qinvokable]
        #[cxx_name = "load"]
        fn load(self: Pin<&mut RuleEditorController>, editable_json: &QString) -> QString;

        /// Check a draft (JSON) as the bridge will; updates `checkJson`.
        #[qinvokable]
        #[cxx_name = "check"]
        fn check(self: Pin<&mut RuleEditorController>, draft_json: &QString);

        /// A valid name for a draft (JSON).
        #[qinvokable]
        #[cxx_name = "suggestName"]
        fn suggest_name(self: &RuleEditorController, draft_json: &QString) -> QString;

        /// Send a draft (JSON). `confirmed`: the user chose "Save anyway"
        /// over its cautions. Returns whether it was sent.
        #[qinvokable]
        fn submit(
            self: Pin<&mut RuleEditorController>,
            draft_json: &QString,
            confirmed: bool,
        ) -> bool;

        /// Give up waiting after a silence (called by a one-second QML timer).
        #[qinvokable]
        fn poll(self: Pin<&mut RuleEditorController>);
    }

    impl cxx_qt::Threading for RuleEditorController {}
}

/// Rust-side state for [`qobject::RuleEditorController`].
#[derive(Default)]
pub struct RuleEditorControllerRust {
    busy: bool,
    status_text: QString,
    editing_name: QString,
    check_json: QString,
    profile_id: QString,
    /// The cached rule being edited, for the cautions.
    old: Option<Value>,
    pending: editor_view::Pending,
}

fn send(message: ClientMessage) -> Result<(), &'static str> {
    let handles = crate::bridge_runtime::handles().ok_or(editor_view::NOT_CONNECTED)?;
    handles
        .try_send(message)
        .map_err(editor_view::not_sent_text)
}

fn draft_json(draft: &RuleDraft) -> QString {
    QString::from(&serde_json::to_string(draft).unwrap_or_default())
}

fn parse_draft(json: &QString) -> Option<RuleDraft> {
    serde_json::from_str(&json.to_string()).ok()
}

impl qobject::RuleEditorController {
    fn set_status(mut self: Pin<&mut Self>, text: &str) {
        self.as_mut().set_status_text(QString::from(text));
    }

    fn start(mut self: Pin<&mut Self>, editing: &str, old: Option<Value>, draft: &RuleDraft) {
        self.as_mut().rust_mut().old = old;
        self.as_mut().set_profile_id(QString::from(""));
        self.as_mut().set_editing_name(QString::from(editing));
        self.as_mut().set_status("");
        self.check_draft(draft);
    }

    fn check_draft(mut self: Pin<&mut Self>, draft: &RuleDraft) -> editor::EditorCheck {
        let profile = self.profile_id.to_string();
        let result = if profile.is_empty() {
            editor::check(draft, self.old.as_ref())
        } else {
            editor_profile::check_profile(draft, &profile)
        };
        let json = serde_json::to_string(&result).unwrap_or_default();
        self.as_mut().set_check_json(QString::from(&json));
        result
    }

    fn catalogue_json(&self) -> QString {
        let presets: Vec<Value> = editor::DURATION_PRESETS
            .iter()
            .map(|(value, label)| serde_json::json!({ "value": value, "label": label }))
            .collect();
        let catalogue = serde_json::json!({
            "operands": editor::operands(),
            "durations": presets,
        });
        QString::from(&catalogue.to_string())
    }

    fn new_rule(self: Pin<&mut Self>) -> QString {
        let draft = editor::new_draft();
        self.start("", None, &draft);
        draft_json(&draft)
    }

    fn new_profile_rule(mut self: Pin<&mut Self>, profile_id: &QString) -> QString {
        let draft = editor::new_draft();
        self.as_mut().start("", None, &draft);
        self.as_mut().set_profile_id(profile_id.clone());
        self.check_draft(&draft);
        draft_json(&draft)
    }

    fn prefill(self: Pin<&mut Self>, form_json: &QString) -> QString {
        let form = serde_json::from_str::<SimulationForm>(&form_json.to_string()).unwrap_or_else(
            |error| {
                tracing::warn!(%error, "RuleEditorController: bad prefill JSON");
                SimulationForm::default()
            },
        );
        let draft = editor::prefill(&form);
        self.start("", None, &draft);
        draft_json(&draft)
    }

    fn load(mut self: Pin<&mut Self>, editable_json: &QString) -> QString {
        let found: Value = serde_json::from_str(&editable_json.to_string()).unwrap_or_default();
        let not_editable = found["notEditable"].as_str().unwrap_or_default();
        let draft = if not_editable.is_empty() {
            RuleDraft::from_wire(&found["rule"])
                .map_err(|reason| format!("{}{reason}", editor_view::NOT_EDITABLE))
        } else {
            Err(not_editable.to_string())
        };
        match draft {
            Ok(draft) => {
                let name = draft.name.clone();
                self.start(&name, Some(found["rule"].clone()), &draft);
                draft_json(&draft)
            }
            Err(reason) => {
                self.as_mut().set_editing_name(QString::from(""));
                self.set_status(&reason);
                QString::from("")
            }
        }
    }

    fn check(self: Pin<&mut Self>, draft_json: &QString) {
        match parse_draft(draft_json) {
            Some(draft) => {
                self.check_draft(&draft);
            }
            None => self.set_status(BAD_DRAFT),
        }
    }

    fn suggest_name(&self, draft_json: &QString) -> QString {
        parse_draft(draft_json)
            .map(|draft| QString::from(&editor::suggest_name(&draft)))
            .unwrap_or_default()
    }

    fn submit(mut self: Pin<&mut Self>, draft_json: &QString, confirmed: bool) -> bool {
        if self.busy {
            return false;
        }
        let Some(draft) = parse_draft(draft_json) else {
            self.set_status(BAD_DRAFT);
            return false;
        };
        let checked = self.as_mut().check_draft(&draft);
        if let Err(reason) = editor_view::may_submit(&checked, confirmed) {
            self.set_status(reason);
            return false;
        }
        let request_id = editor_view::next_request_id("edit");
        let editing = self.editing_name.to_string();
        let profile = self.profile_id.to_string();
        let message = if profile.is_empty() {
            editor_view::submit_message(&draft, &editing, request_id.clone())
        } else {
            editor_profile::profile_message(&draft, &profile, request_id.clone())
        };
        if let Err(reason) = send(message) {
            self.set_status(reason);
            return false;
        }
        self.as_mut()
            .rust_mut()
            .pending
            .sent(request_id, Instant::now());
        self.as_mut().set_busy(true);
        self.set_status(SAVING);
        true
    }

    fn poll(mut self: Pin<&mut Self>) {
        let gave_up = self.as_mut().rust_mut().pending.poll(Instant::now());
        if let Some(done) = gave_up {
            self.finish(done);
        }
    }

    /// The wait ended: say how, and close the sheet if it was saved.
    fn finish(mut self: Pin<&mut Self>, done: editor_view::Finished) {
        self.as_mut().set_busy(false);
        let status = if done.saved && !self.profile_id.is_empty() {
            PROFILE_SAVED
        } else {
            done.status.as_str()
        };
        self.as_mut().set_status(status);
        if done.saved {
            self.saved();
        }
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(message) => self.on_message(message),
            Err(error) => tracing::warn!(%error, "RuleEditorController: bad ServerMessage JSON"),
        }
    }

    fn on_message(mut self: Pin<&mut Self>, message: ServerMessage) {
        let done = self.as_mut().rust_mut().pending.on_message(&message);
        if let Some(done) = done {
            self.finish(done);
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("RuleEditorController: bridge not running; the editor can't save");
            return;
        };
        let qt_thread = self.qt_thread();
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "RuleEditorController",
            editor_view::interests_rule_editor,
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
