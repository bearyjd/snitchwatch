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
const ROLE_CAN_REMOVE: i32 = 7;

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
        /// `received` is false until this session's bridge sends them (an
        /// older bridge never does). `unavailableReason` is why this bridge
        /// changes none (the per-user bridge, or choices it can't read or
        /// save).
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        #[qproperty(bool, received)]
        #[qproperty(QString, unavailable_reason, cxx_name = "unavailableReason")]
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

        /// Ask to turn every listed entry on or off (only those not already
        /// that way).
        #[qinvokable]
        #[cxx_name = "setAll"]
        fn set_all(self: Pin<&mut CuratedDefaultsModel>, on: bool);

        /// Ask to remove an entry's edited rule. Call only after the user
        /// confirmed; ignored for any entry that doesn't offer Remove.
        #[qinvokable]
        #[cxx_name = "removeEntry"]
        fn remove_entry(self: Pin<&mut CuratedDefaultsModel>, id: &QString);
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
            ROLE_CAN_REMOVE => QVariant::from(&self.store.can_remove(entry)),
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
        roles.insert(ROLE_CAN_REMOVE, QByteArray::from("canRemove"));
        roles
    }

    /// From QML: a message for the current session.
    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        let session = self.store.session();
        self.apply_session_message(session, json);
    }

    /// A message from bridge session `connection_id`; a new session starts
    /// from an empty list.
    fn apply_session_message(mut self: Pin<&mut Self>, connection_id: u64, json: &QString) {
        let Ok(msg) = serde_json::from_str::<ServerMessage>(&json.to_string()) else {
            tracing::warn!("CuratedDefaultsModel: bad ServerMessage JSON");
            return;
        };
        let mut next = self.store.clone();
        if !next.apply(connection_id, &msg) {
            return;
        }
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().rust_mut().store = next.clone();
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.as_mut().set_count(next.len() as i32);
        self.as_mut().set_received(next.received());
        self.as_mut()
            .set_unavailable_reason(QString::from(next.unavailable_reason()));
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
                    qobject.apply_session_message(connection_id, &QString::from(&json));
                });
            },
        );
    }

    fn set_entry(self: Pin<&mut Self>, id: &QString, on: bool) {
        let request = self.store.request(&id.to_string(), on);
        self.emit_client(request);
    }

    fn set_all(self: Pin<&mut Self>, on: bool) {
        let request = self.store.request_all(on);
        self.emit_client(request);
    }

    fn remove_entry(self: Pin<&mut Self>, id: &QString) {
        let request = self.store.removal(&id.to_string());
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
