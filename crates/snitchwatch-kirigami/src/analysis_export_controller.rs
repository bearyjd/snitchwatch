//! Session-scoped immutable previews and explicit local saves. No observation data is logged.
use crate::{
    analysis_export::{self, Durability, ExportView, SaveError, SavedArtifact},
    bridge_runtime::BridgeHandles,
};
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use snitchwatch_bridge::{
    analysis_export::{ExportError, ExportFilters},
    ws_messages::{ClientMessage, ServerMessage},
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

const UNAVAILABLE: &str = "Cache summary export requires a connected, updated Snitchwatch service.";
const SCOPE: &str = "Export scope: the bridge's shared current cache, using only the export controls below. Search, grouping and map filters do not apply.";
const UNCERTAIN: &str = "The file was saved, but its durability could not be confirmed.";
const BAD_ARTIFACT: &str = "The service returned an invalid cache summary. Request a new preview.";
const MAX_ARTIFACT: usize = 1_048_576;
type DeliveryGate = Arc<Mutex<Option<(u64, String)>>>;
type Writer = Arc<dyn Fn(&Path, &[u8]) -> Result<SavedArtifact, SaveError> + Send + Sync>;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(bool, available)]
        #[qproperty(bool, busy)]
        #[qproperty(bool, saving)]
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        #[qproperty(QString, scope_text, cxx_name = "scopeText")]
        #[qproperty(QString, preview_text, cxx_name = "previewText")]
        #[qproperty(bool, preview_ready, cxx_name = "previewReady")]
        #[qproperty(QString, saved_location, cxx_name = "savedLocation")]
        type AnalysisExportController = super::AnalysisExportControllerRust;
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut AnalysisExportController>);
        #[qinvokable]
        #[cxx_name = "requestExport"]
        fn request_export(self: Pin<&mut AnalysisExportController>, filters_json: &QString);
        #[qinvokable]
        fn cancel(self: Pin<&mut AnalysisExportController>);
        /// True means the background save was admitted, not that it succeeded.
        #[qinvokable]
        fn save(self: Pin<&mut AnalysisExportController>) -> bool;
        #[qinvokable]
        fn poll(self: Pin<&mut AnalysisExportController>);
    }
    impl cxx_qt::Threading for AnalysisExportController {}
}

pub struct AnalysisExportControllerRust {
    available: bool,
    busy: bool,
    saving: bool,
    status_text: QString,
    scope_text: QString,
    preview_text: QString,
    preview_ready: bool,
    saved_location: QString,
    view: ExportView,
    pending: Option<(u64, String)>,
    session: Option<u64>,
    delivery: DeliveryGate,
    feed_started: bool,
    next_save: u64,
    active_save: Option<u64>,
    writer: Writer,
    #[cfg(test)]
    test_handles: Option<BridgeHandles>,
    #[cfg(test)]
    test_directory: Option<std::path::PathBuf>,
}
impl Default for AnalysisExportControllerRust {
    fn default() -> Self {
        Self {
            available: false,
            busy: false,
            saving: false,
            status_text: QString::from(UNAVAILABLE),
            scope_text: QString::from(SCOPE),
            preview_text: QString::default(),
            preview_ready: false,
            saved_location: QString::default(),
            view: ExportView::default(),
            pending: None,
            session: None,
            delivery: Arc::new(Mutex::new(None)),
            feed_started: false,
            next_save: 0,
            active_save: None,
            writer: Arc::new(analysis_export::save_artifact),
            #[cfg(test)]
            test_handles: None,
            #[cfg(test)]
            test_directory: None,
        }
    }
}
impl AnalysisExportControllerRust {
    fn handles(&self) -> Option<BridgeHandles> {
        #[cfg(test)]
        if let Some(handles) = &self.test_handles {
            return Some(handles.clone());
        }
        crate::bridge_runtime::handles()
    }
    fn directory(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(directory) = &self.test_directory {
            return directory.clone();
        }
        crate::paths::analysis_exports_dir()
    }
}
enum Delivery {
    Artifact(String, Arc<str>),
    Refused(String, ExportError),
    Authenticated,
}
fn interests(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::Authenticated { .. }
            | ServerMessage::CacheSummaryExport { .. }
            | ServerMessage::CacheSummaryExportRefused { .. }
    )
}
fn claim(gate: &DeliveryGate, session: u64, id: &str) -> bool {
    let mut gate = gate.lock().unwrap_or_else(|p| p.into_inner());
    if gate.as_ref().is_some_and(|(s, i)| *s == session && i == id) {
        *gate = None;
        true
    } else {
        false
    }
}
fn request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "export-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn cancel_message(id: String) -> ClientMessage {
    ClientMessage::CancelCacheSummaryExport {
        request_id: id,
        schema_version: 1,
        reply: None,
        session: None,
    }
}
fn refusal(reason: ExportError) -> &'static str {
    match reason {
        ExportError::Busy => "The service is busy. Try again after the current export finishes.",
        ExportError::Cancelled => "The cache summary request was cancelled.",
        ExportError::TimedOut => "The cache summary request timed out. Request a new preview.",
        ExportError::UnsupportedVersion | ExportError::Unavailable => UNAVAILABLE,
        ExportError::InvalidRequest => {
            "The export controls are invalid. Check the UTC window and application selector."
        }
        ExportError::TooLarge => "The cache summary exceeded its safe size limit.",
        ExportError::Internal | ExportError::Collision => {
            "The cache summary could not be prepared. Request a new preview."
        }
    }
}
#[derive(serde::Deserialize)]
struct Metadata {
    format: String,
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    #[serde(rename = "redactionVersion")]
    redaction_version: u64,
    counts: Counts,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    retained: u64,
    inspected: u64,
    accepted_snapshot: u64,
    selected: u64,
    emitted: u64,
    emitted_groups: u64,
}
fn count_scope(artifact: &str) -> Option<String> {
    let metadata = serde_json::from_str::<Metadata>(artifact).ok()?;
    if metadata.format != "snitchwatch-cache-summary"
        || metadata.schema_version != 1
        || metadata.redaction_version != 1
    {
        return None;
    }
    let c = metadata.counts;
    Some(format!("{SCOPE}\nPreview: {} retained; {} inspected; {} accepted into snapshot; {} selected observations; {} emitted observations in {} groups. Coverage is incomplete; unexamined selection is unknown.", c.retained, c.inspected, c.accepted_snapshot, c.selected, c.emitted, c.emitted_groups))
}

impl qobject::AnalysisExportController {
    fn clear_preview(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().view.disconnect();
        self.as_mut().rust_mut().pending = None;
        *self.delivery.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.as_mut().set_busy(false);
        self.as_mut().set_preview_ready(false);
        self.as_mut().set_preview_text(QString::default());
        self.as_mut().set_scope_text(QString::from(SCOPE));
        self.set_saved_location(QString::default());
    }
    fn send_cancel(&self) {
        if let (Some(handles), Some((session, id))) = (self.handles(), &self.pending) {
            let _ = handles.try_send_for_session(*session, cancel_message(id.clone()));
        }
    }
    fn poll(mut self: Pin<&mut Self>) {
        let session = self.handles().and_then(|h| h.analysis_export_session());
        self.as_mut().set_available(session.is_some());
        if self.session != session {
            self.as_mut().rust_mut().session = session;
            self.as_mut().clear_preview();
            if !self.saving {
                self.as_mut()
                    .set_status_text(QString::from(if session.is_some() {
                        "The service session changed. Request a new preview."
                    } else {
                        UNAVAILABLE
                    }));
            }
        }
        if !self.saving && self.as_mut().rust_mut().view.expire(Instant::now()) {
            self.send_cancel();
            self.as_mut().clear_preview();
            self.set_status_text(QString::from(refusal(ExportError::TimedOut)));
        }
    }
    fn start_bridge_feed(mut self: Pin<&mut Self>) {
        self.as_mut().poll();
        if self.feed_started {
            return;
        }
        let Some(handles) = self.handles() else {
            return;
        };
        self.as_mut().rust_mut().feed_started = true;
        let qt = self.qt_thread();
        let guard = handles.clone();
        let gate = self.delivery.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "AnalysisExportController",
            interests,
            move |session, message, _json| {
                let event = match message {
                    ServerMessage::Authenticated { .. } => Delivery::Authenticated,
                    ServerMessage::CacheSummaryExport {
                        request_id,
                        artifact,
                    } if claim(&gate, session, request_id) => {
                        if artifact.len() > MAX_ARTIFACT {
                            Delivery::Refused(request_id.clone(), ExportError::TooLarge)
                        } else {
                            Delivery::Artifact(request_id.clone(), Arc::from(artifact.as_str()))
                        }
                    }
                    ServerMessage::CacheSummaryExportRefused { request_id, reason }
                        if claim(&gate, session, request_id) =>
                    {
                        Delivery::Refused(request_id.clone(), *reason)
                    }
                    _ => return,
                };
                let guard = guard.clone();
                let _ = qt.queue(move |mut object| {
                    if guard.is_current_session(session)
                        && guard.analysis_export_session() == Some(session)
                    {
                        object.as_mut().on_delivery(session, event);
                    } else {
                        object.as_mut().poll();
                    }
                });
            },
        );
    }
    fn request_export(mut self: Pin<&mut Self>, filters_json: &QString) {
        if self.saving {
            return;
        }
        self.as_mut().poll();
        self.send_cancel();
        self.as_mut().clear_preview();
        let Some(handles) = self.handles() else {
            self.set_status_text(QString::from(UNAVAILABLE));
            return;
        };
        let Some(session) = handles.analysis_export_session() else {
            self.set_status_text(QString::from(UNAVAILABLE));
            return;
        };
        let input = filters_json.to_string();
        let filters = (input.len() <= 8192)
            .then(|| serde_json::from_str::<ExportFilters>(&input).ok())
            .flatten();
        let Some(filters) = filters.filter(|f| f.validate().is_ok()) else {
            self.set_status_text(QString::from(refusal(ExportError::InvalidRequest)));
            return;
        };
        let id = request_id();
        let message = ClientMessage::RequestCacheSummaryExport {
            request_id: id.clone(),
            schema_version: 1,
            filters,
            reply: None,
            session: None,
        };
        if !serde_json::to_vec(&message).is_ok_and(|wire| wire.len() <= 8192) {
            self.set_status_text(QString::from(refusal(ExportError::InvalidRequest)));
            return;
        }
        self.as_mut()
            .rust_mut()
            .view
            .begin(session, id.clone(), Instant::now());
        self.as_mut().rust_mut().pending = Some((session, id.clone()));
        *self.delivery.lock().unwrap_or_else(|p| p.into_inner()) = Some((session, id));
        if handles.try_send_for_session(session, message).is_err() {
            self.as_mut().clear_preview();
            self.set_status_text(QString::from("The export request could not be queued. Check the service connection and try again."));
        } else {
            self.as_mut().set_busy(true);
            self.set_status_text(QString::from("Preparing a cache summary preview…"));
        }
    }
    fn on_delivery(mut self: Pin<&mut Self>, session: u64, event: Delivery) {
        if matches!(event, Delivery::Authenticated) {
            self.poll();
            return;
        }
        if self.saving {
            return;
        }
        // Expire before accept/refusal: ExportView::accept can also expire
        // internally, so the controller must finish its own waiting state.
        self.as_mut().poll();
        match event {
            Delivery::Artifact(id, artifact) => {
                if !self
                    .pending
                    .as_ref()
                    .is_some_and(|(s, i)| *s == session && *i == id)
                {
                    return;
                }
                let Some(scope) = count_scope(&artifact) else {
                    self.as_mut().clear_preview();
                    self.set_status_text(QString::from(BAD_ARTIFACT));
                    return;
                };
                if self.as_mut().rust_mut().view.accept(
                    session,
                    &id,
                    artifact.clone(),
                    Instant::now(),
                ) {
                    self.as_mut().set_scope_text(QString::from(&scope));
                    self.as_mut()
                        .set_preview_text(QString::from(artifact.as_ref()));
                    self.as_mut().set_preview_ready(true);
                    self.as_mut().set_busy(false);
                    self.set_status_text(QString::from(
                        "Preview ready. Save explicitly to create a private local file.",
                    ));
                }
            }
            Delivery::Refused(id, reason)
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|(s, i)| *s == session && *i == id) =>
            {
                self.as_mut().clear_preview();
                self.set_status_text(QString::from(refusal(reason)));
            }
            _ => {}
        }
    }
    fn cancel(mut self: Pin<&mut Self>) {
        if self.saving {
            return;
        }
        self.send_cancel();
        self.as_mut().clear_preview();
        self.set_status_text(QString::from("Cache summary preview cancelled."));
    }
    fn save(mut self: Pin<&mut Self>) -> bool {
        if self.saving {
            return false;
        }
        self.as_mut().poll();
        let Some(handles) = self.handles() else {
            return false;
        };
        let Some(session) = handles.analysis_export_session() else {
            return false;
        };
        let Some(artifact) = self.view.artifact_for_save(session) else {
            return false;
        };
        self.as_mut().rust_mut().next_save += 1;
        let job = self.next_save;
        self.as_mut().rust_mut().active_save = Some(job);
        self.as_mut().set_saving(true);
        self.as_mut().set_saved_location(QString::default());
        self.as_mut()
            .set_status_text(QString::from("Saving privately on this device…"));
        let writer = self.writer.clone();
        let directory = self.directory();
        let qt = self.qt_thread();
        let runtime = handles.runtime().clone();
        let worker = runtime.spawn_blocking(move || writer(&directory, artifact.as_bytes()));
        runtime.spawn(async move {
            let outcome = worker.await;
            let _ = qt.queue(move |mut object| {
                if object.active_save != Some(job) { return; }
                object.as_mut().rust_mut().active_save = None;
                object.as_mut().set_saving(false);
                // Local publication has its own lifetime. A lost bridge session
                // clears preview eligibility without discarding this outcome.
                object.as_mut().poll();
                match outcome {
                    Ok(Ok(saved)) => {
                        object.as_mut().set_saved_location(QString::from(saved.path.display().to_string()));
                        object.as_mut().set_status_text(QString::from(if saved.durability == Durability::Unconfirmed { UNCERTAIN } else { "Saved privately on this device." }));
                        // Prevent an accidental second publication from this preview.
                        object.as_mut().rust_mut().view.cancel();
                        object.as_mut().set_preview_ready(false);
                    }
                    Ok(Err(_)) => object.as_mut().set_status_text(QString::from("The file could not be saved. No file was published.")),
                    Err(_) => object.as_mut().set_status_text(QString::from("The save outcome could not be confirmed. Check the local export directory.")),
                }
            });
        });
        true
    }
}

#[cfg(test)]
#[path = "analysis_export_controller/tests.rs"]
mod tests;
