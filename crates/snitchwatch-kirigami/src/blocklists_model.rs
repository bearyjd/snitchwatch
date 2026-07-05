//! `BlocklistsModel` + `BlocklistEntriesModel` — the two `QAbstractListModel`s
//! backing the Blocklists tab (Task 9).
//!
//! The pure fold logic lives in [`crate::blocklists::row_store`] and is
//! unit-tested without Qt. These are the thin cxx-qt wrappers:
//!   * `BlocklistsModel` exposes the subscription master list, plus
//!     subscribe/unsubscribe invokables that emit the bridge's typed
//!     `ClientMessage` (JSON) for the live feed to forward — mirroring
//!     `PendingDecision`'s pattern, no bridge changes.
//!   * `BlocklistEntriesModel` exposes the per-subscription entry (host) list.
//!
//! Blocklist updates are low-frequency whole-list replaces, so both wrappers
//! bracket every applied change with `beginResetModel`/`endResetModel` (see the
//! row_store module docs for why incremental ranges buy nothing here).

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

use crate::blocklists::row_store::{EntriesStore, SubscriptionsStore};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

// Subscription roles.
const SUB_ID: i32 = 0;
const SUB_DISPLAY_NAME: i32 = 1;
const SUB_URL: i32 = 2;
const SUB_ENTRY_COUNT: i32 = 3;
const SUB_STATUS: i32 = 4;
const SUB_LAST_UPDATED: i32 = 5;
const SUB_LAST_FAILURE: i32 = 6;

// Entry roles.
const ENTRY_HOST: i32 = 0;

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
        /// Blocklist subscription master list, bound by `BlocklistsPage.qml`.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        type BlocklistsModel = super::BlocklistsModelRust;

        /// Emitted with a JSON-encoded `ClientMessage` (SubscribeBlocklist /
        /// UnsubscribeBlocklist) for the live bridge feed to forward.
        #[qsignal]
        #[cxx_name = "subscriptionRequested"]
        fn subscription_requested(self: Pin<&mut BlocklistsModel>, json: QString);

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &BlocklistsModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &BlocklistsModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &BlocklistsModel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut BlocklistsModel>, json: &QString);

        /// Subscribe to a new blocklist URL (emits SubscribeBlocklist).
        #[qinvokable]
        fn subscribe(self: Pin<&mut BlocklistsModel>, url: &QString);

        /// Unsubscribe an existing blocklist by id (emits UnsubscribeBlocklist).
        #[qinvokable]
        fn unsubscribe(self: Pin<&mut BlocklistsModel>, id: &QString);

        /// Per-subscription entry (host) list, bound by the detail view.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        #[qproperty(QString, subscription_id, cxx_name = "subscriptionId")]
        type BlocklistEntriesModel = super::BlocklistEntriesModelRust;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &BlocklistEntriesModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &BlocklistEntriesModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &BlocklistEntriesModel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut BlocklistEntriesModel>, json: &QString);

        /// Clear the detail list (e.g. when the selection is cleared).
        #[qinvokable]
        #[cxx_name = "clearEntries"]
        fn clear_entries(self: Pin<&mut BlocklistEntriesModel>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut BlocklistsModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut BlocklistsModel>);

        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut BlocklistEntriesModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut BlocklistEntriesModel>);
    }
}

// --- Subscriptions model -----------------------------------------------------

/// Rust-side state for [`qobject::BlocklistsModel`].
#[derive(Default)]
pub struct BlocklistsModelRust {
    store: SubscriptionsStore,
    count: i32,
}

impl qobject::BlocklistsModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.store.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(sub) = self.store.row(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            SUB_ID => QVariant::from(&QString::from(&sub.id)),
            SUB_DISPLAY_NAME => QVariant::from(&QString::from(&sub.display_name)),
            SUB_URL => QVariant::from(&QString::from(&sub.url)),
            SUB_ENTRY_COUNT => QVariant::from(&(sub.entry_count as i32)),
            SUB_STATUS => QVariant::from(&QString::from(&sub.status)),
            SUB_LAST_UPDATED => QVariant::from(&QString::from(
                sub.last_updated_iso8601.as_deref().unwrap_or(""),
            )),
            SUB_LAST_FAILURE => QVariant::from(&QString::from(
                sub.last_failure_reason.as_deref().unwrap_or(""),
            )),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(SUB_ID, QByteArray::from("listId"));
        roles.insert(SUB_DISPLAY_NAME, QByteArray::from("displayName"));
        roles.insert(SUB_URL, QByteArray::from("url"));
        roles.insert(SUB_ENTRY_COUNT, QByteArray::from("entryCount"));
        roles.insert(SUB_STATUS, QByteArray::from("status"));
        roles.insert(SUB_LAST_UPDATED, QByteArray::from("lastUpdated"));
        roles.insert(SUB_LAST_FAILURE, QByteArray::from("lastFailureReason"));
        roles
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "BlocklistsModel: bad ServerMessage JSON"),
        }
    }

    fn subscribe(self: Pin<&mut Self>, url: &QString) {
        self.emit_client(ClientMessage::SubscribeBlocklist {
            url: url.to_string(),
        });
    }

    fn unsubscribe(self: Pin<&mut Self>, id: &QString) {
        self.emit_client(ClientMessage::UnsubscribeBlocklist { id: id.to_string() });
    }
}

impl qobject::BlocklistsModel {
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: ServerMessage) {
        let changed = {
            unsafe {
                self.as_mut().begin_reset_model();
            }
            let changed = self.as_mut().rust_mut().store.apply(&msg);
            unsafe {
                self.as_mut().end_reset_model();
            }
            changed
        };
        if changed {
            let n = self.store.len() as i32;
            self.as_mut().set_count(n);
        }
    }

    fn emit_client(mut self: Pin<&mut Self>, msg: ClientMessage) {
        match serde_json::to_string(&msg) {
            Ok(json) => self.as_mut().subscription_requested(QString::from(&json)),
            Err(e) => {
                tracing::error!(error = %e, "BlocklistsModel: client message serialize failed")
            }
        }
    }
}

// --- Entries model -----------------------------------------------------------

/// Rust-side state for [`qobject::BlocklistEntriesModel`].
#[derive(Default)]
pub struct BlocklistEntriesModelRust {
    store: EntriesStore,
    count: i32,
    subscription_id: QString,
}

impl qobject::BlocklistEntriesModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.store.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(host) = self.store.host(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            ENTRY_HOST => QVariant::from(&QString::from(host)),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ENTRY_HOST, QByteArray::from("host"));
        roles
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "BlocklistEntriesModel: bad ServerMessage JSON"),
        }
    }

    fn clear_entries(mut self: Pin<&mut Self>) {
        unsafe {
            self.as_mut().begin_reset_model();
        }
        let changed = self.as_mut().rust_mut().store.clear();
        unsafe {
            self.as_mut().end_reset_model();
        }
        if changed {
            self.as_mut().set_count(0);
            self.as_mut().set_subscription_id(QString::default());
        }
    }
}

impl qobject::BlocklistEntriesModel {
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: ServerMessage) {
        unsafe {
            self.as_mut().begin_reset_model();
        }
        let changed = self.as_mut().rust_mut().store.apply(&msg);
        unsafe {
            self.as_mut().end_reset_model();
        }
        if changed {
            let n = self.store.len() as i32;
            let sub = QString::from(self.store.subscription_id());
            self.as_mut().set_count(n);
            self.as_mut().set_subscription_id(sub);
        }
    }
}
