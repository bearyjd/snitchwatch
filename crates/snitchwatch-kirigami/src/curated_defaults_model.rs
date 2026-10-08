//! `CuratedDefaultsModel` — the `QAbstractListModel` behind the Recommended
//! rules page (prompt-slot D). The state is [`crate::curated_defaults`]'s,
//! unit-tested without Qt; this wrapper exposes it as rows and properties
//! and emits the bridge's `SetCuratedDefaults` request as JSON for the live
//! feed to forward (the `ProfilesModel` pattern: no local change, the row
//! follows the bridge's next message).

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

use crate::curated_defaults::{status_text, CuratedStore};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

const ROLE_ID: i32 = 0;
const ROLE_PROGRAM: i32 = 1;
const ROLE_ALLOWS: i32 = 2;
const ROLE_WHY: i32 = 3;
const ROLE_ON: i32 = 4;
const ROLE_STATUS: i32 = 5;
const ROLE_PROBLEM: i32 = 6;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;

        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;
    }

    extern "RustQt" {
        /// The recommended rules, bound by `RecommendedRulesPage.qml`.
        /// `received` is false until the bridge sends them (an older bridge
        /// never does). `unavailableReason` is why this bridge never adds
        /// them; `choicesNotSaved`/`storageReason` say a save failed.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        #[qproperty(bool, received)]
        #[qproperty(QString, unavailable_reason, cxx_name = "unavailableReason")]
        #[qproperty(bool, choices_not_saved, cxx_name = "choicesNotSaved")]
        #[qproperty(QString, storage_reason, cxx_name = "storageReason")]
        type CuratedDefaultsModel = super::CuratedDefaultsModelRust;

        /// Emitted with a JSON-encoded `SetCuratedDefaults` request.
        #[qsignal]
        #[cxx_name = "curatedChangeRequested"]
        fn curated_change_requested(self: Pin<&mut CuratedDefaultsModel>, json: QString);

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &CuratedDefaultsModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &CuratedDefaultsModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &CuratedDefaultsModel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut CuratedDefaultsModel>, json: &QString);

        /// Start the live feed. No-op without a bridge.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut CuratedDefaultsModel>);

        /// Ask to turn one entry on or off.
        #[qinvokable]
        #[cxx_name = "setEntry"]
        fn set_entry(self: Pin<&mut CuratedDefaultsModel>, id: &QString, on: bool);

        /// Ask to turn every listed entry on or off.
        #[qinvokable]
        #[cxx_name = "setAll"]
        fn set_all(self: Pin<&mut CuratedDefaultsModel>, on: bool);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut CuratedDefaultsModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut CuratedDefaultsModel>);
    }

    impl cxx_qt::Threading for CuratedDefaultsModel {}
}

/// Rust-side state for [`qobject::CuratedDefaultsModel`].
#[derive(Default)]
pub struct CuratedDefaultsModelRust {
    store: CuratedStore,
    count: i32,
    received: bool,
    unavailable_reason: QString,
    choices_not_saved: bool,
    storage_reason: QString,
}

impl qobject::CuratedDefaultsModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.store.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(entry) = self.store.row(index.row() as usize) else {
            return QVariant::default();
        };
        let text = |s: &str| QVariant::from(&QString::from(s));
        match role {
            ROLE_ID => text(&entry.id),
            ROLE_PROGRAM => text(&entry.program),
            ROLE_ALLOWS => text(&entry.allows),
            ROLE_WHY => text(&entry.why),
            ROLE_ON => QVariant::from(&entry.on),
            ROLE_STATUS => text(status_text(entry.status)),
            ROLE_PROBLEM => text(entry.problem.as_deref().unwrap_or_default()),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_ID, QByteArray::from("entryId"));
        roles.insert(ROLE_PROGRAM, QByteArray::from("program"));
        roles.insert(ROLE_ALLOWS, QByteArray::from("allows"));
        roles.insert(ROLE_WHY, QByteArray::from("why"));
        // Not `checked`/`on`: avoid names a delegate's controls declare.
        roles.insert(ROLE_ON, QByteArray::from("isOn"));
        roles.insert(ROLE_STATUS, QByteArray::from("statusText"));
        roles.insert(ROLE_PROBLEM, QByteArray::from("problem"));
        roles
    }

    fn apply_server_message_json(mut self: Pin<&mut Self>, json: &QString) {
        let Ok(msg) = serde_json::from_str::<ServerMessage>(&json.to_string()) else {
            tracing::warn!("CuratedDefaultsModel: bad ServerMessage JSON");
            return;
        };
        if !matches!(msg, ServerMessage::SetCuratedDefaults { .. }) {
            return;
        }
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().rust_mut().store.apply(&msg);
        unsafe {
            self.as_mut().end_reset_model();
        }
        let store = self.store.clone();
        self.as_mut().set_count(store.len() as i32);
        self.as_mut().set_received(store.received());
        self.as_mut()
            .set_unavailable_reason(QString::from(store.unavailable_reason()));
        self.as_mut()
            .set_choices_not_saved(store.choices_not_saved());
        self.as_mut()
            .set_storage_reason(QString::from(store.storage_reason()));
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("CuratedDefaultsModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "CuratedDefaultsModel",
            crate::bridge_dispatch::interests_curated_defaults,
            move |connection_id, _msg, json| {
                let session_handles = session_handles.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if !session_handles.is_current_session(connection_id) {
                        return;
                    }
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );
    }

    fn set_entry(self: Pin<&mut Self>, id: &QString, on: bool) {
        let id = id.to_string();
        let request = self.store.request(&[id.as_str()], on);
        self.emit_client(request);
    }

    fn set_all(self: Pin<&mut Self>, on: bool) {
        let request = self.store.request(&self.store.ids(), on);
        self.emit_client(request);
    }

    fn emit_client(mut self: Pin<&mut Self>, msg: Option<ClientMessage>) {
        let Some(msg) = msg else {
            tracing::warn!("CuratedDefaultsModel: request ignored (not offered here)");
            return;
        };
        match serde_json::to_string(&msg) {
            Ok(json) => self.as_mut().curated_change_requested(QString::from(&json)),
            Err(e) => tracing::error!(error = %e, "CuratedDefaultsModel: serialize failed"),
        }
    }
}
