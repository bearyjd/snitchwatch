//! `RulesIoController` — rule export and import for `RulesPage.qml`
//! (roadmap P2.7).
//!
//! The GUI reads and writes the files (the system bridge runs as its own
//! user and can't reach a home directory; under Flatpak the file dialogs go
//! through the portal); the bridge validates. The Qt-free parts live in
//! [`crate::rules::io`] (the bounded read, the owner-only write) and
//! [`crate::rules::io_view`] (the preview rows and texts).
//!
//! The bridge answers only the connection that asked, echoing each
//! request's id (an apply's progress and result carry its preview id); this
//! controller still acts only on the answer it is waiting for, from the live
//! bridge session. A preview is applied on the session it came from
//! (`try_send_for_session`), so a reconnect can't apply it to another
//! bridge. Per-rule outcomes are collected and handed to QML on the
//! one-second poll. Every string it exposes is plain text for
//! `Text.PlainText` labels.

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QUrl};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::bridge_runtime::SendClientMessageError;
use crate::rules::{io, io_view};
use snitchwatch_bridge::rule_io::{Document, OmittedCounts};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

/// How long to wait for the bridge between answers before giving up.
const NO_ANSWER_AFTER: Duration = Duration::from_secs(30);

const NOT_CONNECTED: &str = "Snitchwatch isn't connected to its service, so nothing was sent.";
const STALE_SESSION: &str = "The connection to the Snitchwatch service was reset after the \
     preview. Import the file again.";
const QUEUE_FULL: &str = "Snitchwatch is busy. Try again in a moment.";
const NO_ANSWER: &str = "No answer from the Snitchwatch service. It may be too old to import \
     or export rules, or the answer was lost; try again.";
const NO_RESULT: &str = "The import's result wasn't received. Check the Rules page for what \
     was applied.";
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

        /// The save dialog was cancelled: drop the export and say so.
        #[qinvokable]
        #[cxx_name = "exportCancelled"]
        fn export_cancelled(self: Pin<&mut RulesIoController>);

        /// Read `url` (at most the import cap, a regular file, JSON) and ask
        /// the bridge for a preview.
        #[qinvokable]
        #[cxx_name = "readImport"]
        fn read_import(self: Pin<&mut RulesIoController>, url: &QUrl) -> bool;

        /// Apply the named rules of the current preview (`names_json`: a JSON
        /// array of the ticked rows' names).
        #[qinvokable]
        fn apply(self: Pin<&mut RulesIoController>, names_json: &QString) -> bool;

        /// Hand collected outcomes to QML, and give up waiting after a
        /// silence (called by a one-second QML timer).
        #[qinvokable]
        fn poll(self: Pin<&mut RulesIoController>);
    }

    impl cxx_qt::Threading for RulesIoController {}
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Waiting {
    #[default]
    Nothing,
    /// An answer to this request id.
    Request(String),
    /// The progress and result of applying a preview (or the apply's
    /// refusal).
    Apply {
        preview_id: String,
        request_id: String,
    },
}

/// A received export, until it is saved or dropped.
struct PendingExport {
    text: String,
    rules: usize,
    omitted: OmittedCounts,
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
    export: Option<PendingExport>,
    /// The preview's bridge session and id.
    preview: Option<(Option<u64>, String)>,
    progress: io_view::ProgressLog,
}

fn next_request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
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
        self.as_mut().flush_progress();
        self.as_mut().rust_mut().waiting = Waiting::Nothing;
        self.as_mut().rust_mut().last_answer = None;
        self.as_mut().set_busy(false);
        self.as_mut().set_applying(false);
        self.set_status(status);
    }

    fn flush_progress(mut self: Pin<&mut Self>) {
        if let Some(json) = self.as_mut().rust_mut().progress.take_json() {
            self.set_results_json(QString::from(&json));
        }
    }

    fn request_export(mut self: Pin<&mut Self>) {
        if self.busy {
            return;
        }
        self.as_mut().rust_mut().export = None;
        let request_id = next_request_id();
        let message = ClientMessage::ExportRules {
            request_id: request_id.clone(),
            reply: None,
        };
        match send(message, None) {
            Ok(()) => self.begin(
                Waiting::Request(request_id),
                "Asking the firewall service for its rules…",
            ),
            Err(error) => self.set_status(error),
        }
    }

    fn write_export(mut self: Pin<&mut Self>, url: &QUrl) -> bool {
        let Some(export) = self.as_mut().rust_mut().export.take() else {
            self.set_status(NOTHING_TO_SAVE);
            return false;
        };
        let written = local_path(url)
            .ok_or(io::FileError::NotLocal)
            .and_then(|path| io::write_export_file(Path::new(&path), &export.text));
        match written {
            Ok(()) => {
                self.set_status(&io_view::export_saved(export.rules, &export.omitted));
                true
            }
            Err(error) => {
                let status = io_view::export_failed(error.describe(), &export.omitted);
                self.set_status(&status);
                false
            }
        }
    }

    fn export_cancelled(mut self: Pin<&mut Self>) {
        if self.as_mut().rust_mut().export.take().is_some() {
            self.set_status(io_view::export_cancelled());
        }
    }

    fn read_import(mut self: Pin<&mut Self>, url: &QUrl) -> bool {
        if self.busy {
            return false;
        }
        let request_id = next_request_id();
        let request = local_path(url)
            .ok_or(io::FileError::NotLocal)
            .and_then(|path| io::read_import_file(Path::new(&path)))
            .and_then(|document| io::preview_request(request_id.clone(), document));
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
                self.begin(Waiting::Request(request_id), "Checking the file…");
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
        let request_id = next_request_id();
        let message = ClientMessage::ApplyRulesImport {
            request_id: request_id.clone(),
            preview_id: preview_id.clone(),
            include,
            reply: None,
        };
        match send(message, session) {
            Ok(()) => {
                self.as_mut().rust_mut().progress.clear();
                self.as_mut().flush_progress();
                self.as_mut().set_applying(true);
                let status = format!("Applying {count} rule changes…");
                let waiting = Waiting::Apply {
                    preview_id,
                    request_id,
                };
                self.begin(waiting, &status);
                true
            }
            Err(error) => {
                self.set_status(error);
                false
            }
        }
    }

    fn poll(mut self: Pin<&mut Self>) {
        self.as_mut().flush_progress();
        let silent = self
            .last_answer
            .is_some_and(|at| at.elapsed() > NO_ANSWER_AFTER);
        if self.busy && silent {
            let status = match self.waiting {
                Waiting::Apply { .. } => NO_RESULT,
                _ => NO_ANSWER,
            };
            self.finish(status);
        }
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(message) => self.on_message(message, None),
            Err(error) => tracing::warn!(%error, "RulesIoController: bad ServerMessage JSON"),
        }
    }

    /// Whether `message` answers what this controller waits for.
    fn awaited(&self, message: &ServerMessage) -> bool {
        match (&self.waiting, message) {
            (
                Waiting::Request(id),
                ServerMessage::RulesExport { request_id, .. }
                | ServerMessage::RulesExportUnavailable { request_id, .. }
                | ServerMessage::RulesImportPreview { request_id, .. }
                | ServerMessage::RulesImportRefused { request_id, .. },
            ) => request_id == id,
            (
                Waiting::Apply { request_id, .. },
                ServerMessage::RulesImportRefused { request_id: id, .. },
            ) => id == request_id,
            (
                Waiting::Apply { preview_id, .. },
                ServerMessage::RulesImportProgress { preview_id: id, .. }
                | ServerMessage::RulesImportResult { preview_id: id, .. },
            ) => id == preview_id,
            _ => false,
        }
    }

    fn on_message(mut self: Pin<&mut Self>, message: ServerMessage, session: Option<u64>) {
        if !self.awaited(&message) {
            return;
        }
        self.as_mut().rust_mut().last_answer = Some(Instant::now());
        match message {
            ServerMessage::RulesExport {
                document, omitted, ..
            } => self.on_export(&document, omitted),
            ServerMessage::RulesImportPreview {
                preview_id, items, ..
            } => {
                let view = serde_json::to_string(&io_view::group(&items)).unwrap_or_default();
                self.as_mut().rust_mut().preview = Some((session, preview_id));
                self.as_mut().set_applied(false);
                self.as_mut().set_preview_json(QString::from(&view));
                self.as_mut().finish("");
                self.preview_ready();
            }
            ServerMessage::RulesExportUnavailable { reason, .. }
            | ServerMessage::RulesImportRefused { reason, .. } => self.finish(&reason),
            ServerMessage::RulesImportProgress { name, outcome, .. } => {
                let text = io_view::outcome_text(&outcome);
                self.as_mut().rust_mut().progress.record(name, text);
            }
            ServerMessage::RulesImportResult {
                applied,
                rejected,
                not_sent,
                no_answer,
                ..
            } => {
                self.as_mut().set_applied(true);
                let summary = io_view::result_summary(applied, rejected, not_sent, no_answer);
                self.finish(&summary);
            }
            _ => {}
        }
    }

    fn on_export(mut self: Pin<&mut Self>, document: &Document, omitted: OmittedCounts) {
        let text = io_view::export_text(document);
        let rules = document.rules.len();
        let status = io_view::export_ready(rules, &omitted, text.len());
        self.as_mut().rust_mut().export = Some(PendingExport {
            text,
            rules,
            omitted,
        });
        self.as_mut().finish(&status);
        self.export_ready();
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
            io_view::interests_rules_io,
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
