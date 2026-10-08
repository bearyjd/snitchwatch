//! `RulesIoController` — rule export and import for `RulesPage.qml`
//! (roadmap P2.7).
//!
//! The GUI reads and writes the files (the system bridge runs as its own
//! user and can't reach a home directory; under Flatpak the file dialogs go
//! through the portal); the bridge validates. The Qt-free parts live in
//! [`crate::rules::io`]: the bounded read, the owner-only write, the
//! preview's grouping and default ticks.
//!
//! Every `ServerMessage` is broadcast to every GUI, so this controller acts
//! only on the answer it is waiting for, and only from the live bridge
//! session. A preview is applied on the session it came from
//! (`try_send_for_session`), so a reconnect can't apply it to another
//! bridge. Every string it exposes is plain text for `Text.PlainText`
//! labels.

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QUrl};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::bridge_runtime::SendClientMessageError;
use crate::rules::io;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

/// How long to wait for the bridge between answers before giving up.
const NO_ANSWER_AFTER: Duration = Duration::from_secs(30);

const NOT_CONNECTED: &str = "Snitchwatch isn't connected to its service, so nothing was sent.";
const STALE_SESSION: &str =
    "The Snitchwatch service restarted since the preview. Import the file again.";
const QUEUE_FULL: &str = "Snitchwatch is busy. Try again in a moment.";
const NO_ANSWER: &str = "No answer from the Snitchwatch service. It may be too old to import \
     or export rules.";
const NOTHING_TO_SAVE: &str = "There is no export to save. Export again.";
const NO_PREVIEW: &str = "There is no import preview to apply. Import the file again.";

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    extern "RustQt" {
        /// Export/import state for `RulesPage.qml` and `RulesImportSheet.qml`.
        #[qobject]
        #[qml_element]
        /// Waiting for the bridge; the page's buttons are disabled.
        #[qproperty(bool, busy)]
        /// The last outcome or error, as plain text.
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        /// `rules::io::PreviewView` as JSON, or empty.
        #[qproperty(QString, preview_json, cxx_name = "previewJson")]
        /// `{ rule name: outcome text }` for the apply in progress, as JSON.
        #[qproperty(QString, results_json, cxx_name = "resultsJson")]
        /// An apply was sent and its result hasn't arrived.
        #[qproperty(bool, applying)]
        /// The preview was applied; it can't be applied again.
        #[qproperty(bool, applied)]
        type RulesIoController = super::RulesIoControllerRust;

        /// The export arrived; QML opens the save dialog.
        #[qsignal]
        #[cxx_name = "exportReady"]
        fn export_ready(self: Pin<&mut RulesIoController>);

        /// The preview arrived; QML opens the import sheet.
        #[qsignal]
        #[cxx_name = "previewReady"]
        fn preview_ready(self: Pin<&mut RulesIoController>);

        /// Listen for the bridge's import/export answers. No-op headless.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut RulesIoController>);

        /// Handle one `ServerMessage` (JSON). The live feed calls this; so do
        /// headless QML probes.
        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut RulesIoController>, json: &QString);

        /// Ask the bridge for an export (`ExportRules`).
        #[qinvokable]
        #[cxx_name = "requestExport"]
        fn request_export(self: Pin<&mut RulesIoController>);

        /// Save the received export to `url`, owner-only (0600).
        #[qinvokable]
        #[cxx_name = "writeExport"]
        fn write_export(self: Pin<&mut RulesIoController>, url: &QUrl) -> bool;

        /// Read `url` (at most the import cap, a regular file, JSON) and ask
        /// the bridge for a preview.
        #[qinvokable]
        #[cxx_name = "readImport"]
        fn read_import(self: Pin<&mut RulesIoController>, url: &QUrl) -> bool;

        /// Apply the named rules of the current preview (`names_json`: a JSON
        /// array of the ticked rows' names).
        #[qinvokable]
        fn apply(self: Pin<&mut RulesIoController>, names_json: &QString) -> bool;

        /// Give up waiting after a silence (called by a QML timer).
        #[qinvokable]
        fn poll(self: Pin<&mut RulesIoController>);
    }

    impl cxx_qt::Threading for RulesIoController {}
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Waiting {
    #[default]
    Nothing,
    Export,
    Preview,
    Apply,
}

/// Rust-side state for [`qobject::RulesIoController`].
#[derive(Default)]
pub struct RulesIoControllerRust {
    busy: bool,
    status_text: QString,
    preview_json: QString,
    results_json: QString,
    applying: bool,
    applied: bool,
    waiting: Waiting,
    last_answer: Option<Instant>,
    /// The received export, pretty-printed, until it is saved.
    export_text: Option<String>,
    /// The preview's bridge session and id.
    preview: Option<(Option<u64>, String)>,
    results: BTreeMap<String, String>,
}

fn send(message: ClientMessage, session: Option<u64>) -> Result<(), &'static str> {
    let handles = crate::bridge_runtime::handles().ok_or(NOT_CONNECTED)?;
    let sent = match session {
        Some(id) => handles.try_send_for_session(id, message),
        None => handles.try_send(message),
    };
    sent.map_err(|error| match error {
        SendClientMessageError::Disconnected | SendClientMessageError::Stopped => NOT_CONNECTED,
        SendClientMessageError::StaleSession => STALE_SESSION,
        SendClientMessageError::Full => QUEUE_FULL,
    })
}

fn local_path(url: &QUrl) -> Option<String> {
    url.to_local_file().map(|path| path.to_string())
}

impl qobject::RulesIoController {
    fn set_status(mut self: Pin<&mut Self>, text: &str) {
        self.as_mut().set_status_text(QString::from(text));
    }

    fn begin(mut self: Pin<&mut Self>, waiting: Waiting, status: &str) {
        self.as_mut().rust_mut().waiting = waiting;
        self.as_mut().rust_mut().last_answer = Some(Instant::now());
        self.as_mut().set_busy(true);
        self.set_status(status);
    }

    fn finish(mut self: Pin<&mut Self>, status: &str) {
        self.as_mut().rust_mut().waiting = Waiting::Nothing;
        self.as_mut().rust_mut().last_answer = None;
        self.as_mut().set_busy(false);
        self.as_mut().set_applying(false);
        self.set_status(status);
    }

    fn request_export(mut self: Pin<&mut Self>) {
        if self.busy {
            return;
        }
        self.as_mut().rust_mut().export_text = None;
        match send(ClientMessage::ExportRules, None) {
            Ok(()) => self.begin(
                Waiting::Export,
                "Asking the firewall service for its rules…",
            ),
            Err(error) => self.set_status(error),
        }
    }

    fn write_export(mut self: Pin<&mut Self>, url: &QUrl) -> bool {
        let Some(text) = self.export_text.clone() else {
            self.set_status(NOTHING_TO_SAVE);
            return false;
        };
        let Some(path) = local_path(url) else {
            self.set_status(io::FileError::NotLocal.describe());
            return false;
        };
        match io::write_export_file(Path::new(&path), &text) {
            Ok(()) => {
                self.as_mut().rust_mut().export_text = None;
                let status = format!("{} Saved.", self.status_text);
                self.set_status(&status);
                true
            }
            Err(error) => {
                self.set_status(error.describe());
                false
            }
        }
    }

    fn read_import(mut self: Pin<&mut Self>, url: &QUrl) -> bool {
        if self.busy {
            return false;
        }
        let request = local_path(url)
            .ok_or(io::FileError::NotLocal)
            .and_then(|path| io::read_import_file(Path::new(&path)))
            .and_then(io::preview_request);
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                self.set_status(error.describe());
                return false;
            }
        };
        self.as_mut().rust_mut().preview = None;
        self.as_mut().set_preview_json(QString::from(""));
        match send(request, None) {
            Ok(()) => {
                self.begin(Waiting::Preview, "Checking the file…");
                true
            }
            Err(error) => {
                self.set_status(error);
                false
            }
        }
    }

    fn apply(mut self: Pin<&mut Self>, names_json: &QString) -> bool {
        if self.busy || self.applied {
            return false;
        }
        let Some((session, preview_id)) = self.preview.clone() else {
            self.set_status(NO_PREVIEW);
            return false;
        };
        let Ok(include) = serde_json::from_str::<Vec<String>>(&names_json.to_string()) else {
            tracing::warn!("RulesIoController: apply names are not a JSON string array");
            return false;
        };
        let count = include.len();
        let message = ClientMessage::ApplyRulesImport {
            preview_id,
            include,
        };
        match send(message, session) {
            Ok(()) => {
                self.as_mut().rust_mut().results.clear();
                self.as_mut().set_results_json(QString::from("{}"));
                self.as_mut().set_applying(true);
                let status = format!("Applying {count} rule changes…");
                self.begin(Waiting::Apply, &status);
                true
            }
            Err(error) => {
                self.set_status(error);
                false
            }
        }
    }

    fn poll(self: Pin<&mut Self>) {
        let silent = self
            .last_answer
            .is_some_and(|at| at.elapsed() > NO_ANSWER_AFTER);
        if self.busy && silent {
            self.finish(NO_ANSWER);
        }
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(message) => self.on_message(message, None),
            Err(error) => tracing::warn!(%error, "RulesIoController: bad ServerMessage JSON"),
        }
    }

    fn on_message(mut self: Pin<&mut Self>, message: ServerMessage, session: Option<u64>) {
        match (self.waiting, message) {
            (Waiting::Export, ServerMessage::RulesExport { document, omitted }) => {
                let text = serde_json::to_string_pretty(&document).unwrap_or_default();
                let summary = io::export_summary(document.rules.len(), &omitted, text.len());
                self.as_mut().rust_mut().export_text = Some(text);
                self.as_mut().finish(&summary);
                self.export_ready();
            }
            (Waiting::Export, ServerMessage::RulesExportUnavailable { reason }) => {
                self.finish(&reason)
            }
            (Waiting::Preview, ServerMessage::RulesImportPreview { preview_id, items }) => {
                let view = serde_json::to_string(&io::group(&items)).unwrap_or_default();
                self.as_mut().rust_mut().preview = Some((session, preview_id));
                self.as_mut().set_applied(false);
                self.as_mut().set_preview_json(QString::from(&view));
                self.as_mut().finish("");
                self.preview_ready();
            }
            (Waiting::Preview | Waiting::Apply, ServerMessage::RulesImportRefused { reason }) => {
                self.finish(&reason)
            }
            (Waiting::Apply, ServerMessage::RulesImportProgress { name, outcome }) => {
                self.as_mut().rust_mut().last_answer = Some(Instant::now());
                self.as_mut()
                    .rust_mut()
                    .results
                    .insert(name, io::outcome_text(&outcome));
                let json = serde_json::to_string(&self.results).unwrap_or_default();
                self.set_results_json(QString::from(&json));
            }
            (
                Waiting::Apply,
                ServerMessage::RulesImportResult {
                    applied,
                    rejected,
                    not_sent,
                    no_answer,
                },
            ) => {
                self.as_mut().set_applied(true);
                let summary = io::result_summary(applied, rejected, not_sent, no_answer);
                self.finish(&summary);
            }
            _ => {}
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("RulesIoController: bridge not running; import/export disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "RulesIoController",
            io::interests_rules_io,
            move |connection_id, message, _json| {
                let session_handles = session_handles.clone();
                let message = message.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if session_handles.is_current_session(connection_id) {
                        qobject.on_message(message, Some(connection_id));
                    }
                });
            },
        );
    }
}
