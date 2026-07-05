//! `ConnectionsModel` — a `QAbstractListModel` exposing the connection list to
//! QML (Task 6).
//!
//! The pure ordering/CRUD logic lives in [`super::row_store::RowStore`] and is
//! unit-tested without Qt. This module is the thin cxx-qt wrapper that:
//!   * exposes rows to a QML `ListView` via role names, and
//!   * replays [`RowStore`] mutations behind the correct Qt begin/end row
//!     signals so the view updates incrementally (no full reset on every new
//!     connection — important so the core-loop list doesn't steal focus/scroll
//!     from a row the user is investigating).
//!
//! Live wiring — a Tokio task subscribing to the bridge's typed
//! `broadcast::Receiver<ServerMessage>` and calling `qt_thread.queue(|m|
//! m.apply_server_message(...))` per the Task 1 async pattern — attaches via
//! [`qobject::ConnectionsModel::apply_server_message`]. Exposing that receiver
//! from `RunningBridge` is a small consumer-side follow-up (the bridge's WS
//! protocol itself is unchanged, per the plan's non-goals).

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QVariant,
};
use tokio::sync::broadcast;

use crate::connections::row_store::{ModelOp, RowStore, Verdict};
use snitchwatch_bridge::ws_messages::{ConnectionRow, ServerMessage};

// Role ids exposed to the QML delegate.
const ROLE_ID: i32 = 0;
const ROLE_PROCESS: i32 = 1;
const ROLE_HOST: i32 = 2;
const ROLE_PORT: i32 = 3;
const ROLE_PROTOCOL: i32 = 4;
const ROLE_VERDICT: i32 = 5;
const ROLE_PENDING: i32 = 6;

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
        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;

        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;
    }

    extern "RustQt" {
        /// Connection list model bound by `ConnectionsPage.qml`'s `ListView`.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        /// Number of rows currently *visible* (after any active filter). This
        /// is what the bound `ListView` shows.
        #[qproperty(i32, count)]
        /// Number of rows still awaiting a decision, across the *whole* store
        /// (independent of the filter) — drives the pending badge/tray.
        #[qproperty(i32, pending_count, cxx_name = "pendingCount")]
        /// Total rows in the store regardless of filter — lets the view tell
        /// "no connections yet" apart from "no rows match the filter".
        #[qproperty(i32, total_count, cxx_name = "totalCount")]
        type ConnectionsModel = super::ConnectionsModelRust;

        /// Emitted when the auto-select policy decides a freshly arrived pending
        /// row should take the selection. `row` is the *visible* index to
        /// select. A signal (not a property) so it always fires — even when the
        /// target index equals the current one — and never steals focus except
        /// when the policy says to. QML connects it to `ListView.currentIndex`.
        #[qsignal]
        #[cxx_name = "autoSelectRequested"]
        fn auto_select_requested(self: Pin<&mut ConnectionsModel>, row: i32);

        // --- QAbstractListModel overrides -----------------------------------
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &ConnectionsModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &ConnectionsModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &ConnectionsModel) -> QHash_i32_QByteArray;

        // --- Feed ingestion (QML/string convenience) ------------------------
        /// Apply one JSON-encoded bridge `ServerMessage`. Convenience wrapper
        /// over the typed `apply_server_message`, callable from QML; malformed
        /// input is logged and ignored.
        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut ConnectionsModel>, json: &QString);

        /// Start the live outbound feed (Task 13): subscribe to the bridge's
        /// `ServerMessage` broadcast and queue connection-relevant messages onto
        /// the Qt thread via `applyServerMessageJson`. No-op when the bridge
        /// isn't running (e.g. headless tests). Called from QML
        /// `Component.onCompleted`; safe to call at most once per instance.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut ConnectionsModel>);

        // --- Filter / search (Task 8) --------------------------------------
        /// Set the case-insensitive substring search query. Empty clears it.
        #[qinvokable]
        #[cxx_name = "setFilterQuery"]
        fn set_filter_query(self: Pin<&mut ConnectionsModel>, query: &QString);

        /// Toggle "show only pending rows".
        #[qinvokable]
        #[cxx_name = "setPendingOnly"]
        fn set_pending_only(self: Pin<&mut ConnectionsModel>, pending_only: bool);

        // --- Selection tracking (Task 8 auto-select) ------------------------
        /// Tell the model which row the user currently has selected (empty =
        /// none), so the auto-select policy can avoid stealing focus from a
        /// pending row the user is investigating.
        #[qinvokable]
        #[cxx_name = "setCurrentRowId"]
        fn set_current_row_id(self: Pin<&mut ConnectionsModel>, id: &QString);
    }

    // Protected base-class helpers inherited from QAbstractListModel.
    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(
            self: Pin<&mut ConnectionsModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut ConnectionsModel>);

        #[inherit]
        #[cxx_name = "beginRemoveRows"]
        unsafe fn begin_remove_rows(
            self: Pin<&mut ConnectionsModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        #[cxx_name = "endRemoveRows"]
        unsafe fn end_remove_rows(self: Pin<&mut ConnectionsModel>);

        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut ConnectionsModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut ConnectionsModel>);

        #[inherit]
        fn index(
            self: &ConnectionsModel,
            row: i32,
            column: i32,
            parent: &QModelIndex,
        ) -> QModelIndex;

        #[inherit]
        #[cxx_name = "dataChanged"]
        fn data_changed(
            self: Pin<&mut ConnectionsModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }

    impl cxx_qt::Threading for ConnectionsModel {}
}

/// Rust-side state for [`qobject::ConnectionsModel`].
#[derive(Default)]
pub struct ConnectionsModelRust {
    store: RowStore,
    count: i32,
    pending_count: i32,
    total_count: i32,
    /// Id of the row the user currently has selected (empty = none). Fed from
    /// QML via `setCurrentRowId`; consulted by the auto-select policy.
    current_row_id: String,
}

impl qobject::ConnectionsModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.store.visible_len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(row) = self.store.visible_row(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            ROLE_ID => QVariant::from(&QString::from(&row.id)),
            ROLE_PROCESS => QVariant::from(&QString::from(&row.process)),
            ROLE_HOST => QVariant::from(&QString::from(&row.dst_host)),
            ROLE_PORT => QVariant::from(&(row.dst_port as i32)),
            ROLE_PROTOCOL => QVariant::from(&QString::from(&row.protocol)),
            ROLE_VERDICT => {
                let v = Verdict::from_action(row.action.as_deref());
                QVariant::from(&QString::from(v.as_token()))
            }
            ROLE_PENDING => QVariant::from(&row.action.is_none()),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_ID, QByteArray::from("rowId"));
        roles.insert(ROLE_PROCESS, QByteArray::from("process"));
        roles.insert(ROLE_HOST, QByteArray::from("host"));
        roles.insert(ROLE_PORT, QByteArray::from("port"));
        roles.insert(ROLE_PROTOCOL, QByteArray::from("protocol"));
        roles.insert(ROLE_VERDICT, QByteArray::from("verdict"));
        roles.insert(ROLE_PENDING, QByteArray::from("pending"));
        roles
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "ConnectionsModel: bad ServerMessage JSON"),
        }
    }

    fn set_filter_query(self: Pin<&mut Self>, query: &QString) {
        let mut filter = self.store.filter().clone();
        filter.set_query(query.to_string());
        self.apply_filter(filter);
    }

    fn set_pending_only(self: Pin<&mut Self>, pending_only: bool) {
        let mut filter = self.store.filter().clone();
        filter.set_pending_only(pending_only);
        self.apply_filter(filter);
    }

    fn set_current_row_id(mut self: Pin<&mut Self>, id: &QString) {
        self.as_mut().rust_mut().current_row_id = id.to_string();
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("ConnectionsModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let mut rx = handles.subscribe();
        handles.runtime().spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if !crate::bridge_dispatch::interests_connections(&msg) {
                            continue;
                        }
                        match crate::bridge_dispatch::encode_server(&msg) {
                            Ok(json) => {
                                let _ = qt_thread.queue(move |qobject| {
                                    qobject.apply_server_message_json(&QString::from(&json));
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "ConnectionsModel feed: encode failed")
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "ConnectionsModel feed lagged behind bridge")
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

impl qobject::ConnectionsModel {
    /// Apply a typed bridge message, replaying each change behind the correct
    /// Qt begin/end signals. This is the direct-typed-API path (no JSON
    /// round-trip); `apply_server_message_json` is the QML string wrapper.
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: ServerMessage) {
        // Ids that arrive *pending* in this batch — captured before mutation so
        // the auto-select policy can react to them afterward.
        let new_pending_ids: Vec<String> = match &msg {
            ServerMessage::InsertConnectionRows { rows } => rows
                .iter()
                .filter(|r| r.action.is_none())
                .map(|r| r.id.clone())
                .collect(),
            _ => Vec::new(),
        };

        if self.store.is_filtered() {
            // With an active filter the visible→store index mapping shifts in
            // ways that don't map cleanly onto incremental begin/insert/remove
            // brackets; a filter is an explicit user investigation mode, so a
            // reset is acceptable here (and doesn't affect the unfiltered
            // core-loop path below, which stays incremental to preserve scroll
            // position). Recompute the projection inside the reset bracket.
            unsafe {
                self.as_mut().begin_reset_model();
            }
            {
                let mut rust = self.as_mut().rust_mut();
                rust.store.apply(msg);
                rust.store.recompute_visible();
            }
            unsafe {
                self.as_mut().end_reset_model();
            }
        } else {
            match msg {
                ServerMessage::InsertConnectionRows { rows } => self.as_mut().apply_insert(rows),
                ServerMessage::RemoveConnectionRows { ids } => self.as_mut().apply_remove(&ids),
                ServerMessage::UpdateConnectionRows { rows } => self.as_mut().apply_update(rows),
                other => {
                    // Move/Clear (and no-op variants): bracket with a full reset
                    // — rare relative to insert/update, and always view-correct.
                    unsafe {
                        self.as_mut().begin_reset_model();
                    }
                    self.as_mut().rust_mut().store.apply(other);
                    unsafe {
                        self.as_mut().end_reset_model();
                    }
                }
            }
        }
        self.as_mut().refresh_counts();
        self.maybe_auto_select(&new_pending_ids);
    }

    /// Replace the active filter, bracketed by a model reset (the whole visible
    /// set changes), and refresh the derived counts.
    fn apply_filter(
        mut self: Pin<&mut Self>,
        filter: crate::connections::filter::ConnectionFilter,
    ) {
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().rust_mut().store.set_filter(filter);
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.refresh_counts();
    }

    /// Apply the auto-select-on-new-pending-row policy. Sets `autoSelectRowId`
    /// (which QML watches) when a freshly arrived pending row should take the
    /// selection, without stealing focus from a pending row the user is on.
    fn maybe_auto_select(mut self: Pin<&mut Self>, new_pending_ids: &[String]) {
        if new_pending_ids.is_empty() {
            return;
        }
        let current = self.current_row_id.clone();
        let current_opt = if current.is_empty() {
            None
        } else {
            Some(current.as_str())
        };
        let current_is_pending = current_opt
            .and_then(|id| self.store.is_pending(id))
            .unwrap_or(false);
        let refs: Vec<&str> = new_pending_ids.iter().map(String::as_str).collect();
        let Some(target) =
            crate::connections::filter::auto_select_target(current_opt, current_is_pending, &refs)
        else {
            return;
        };
        // Translate the chosen id to its current *visible* index for QML. If the
        // target is filtered out of view there's nothing to select.
        if let Some(vis) = self.store.visible_index_of(&target) {
            // Adopt the auto-selected row as the current selection so the *next*
            // pending arrival won't jump the selection off it (surface one
            // pending decision at a time, per the design spec).
            self.as_mut().rust_mut().current_row_id = target;
            self.as_mut().auto_select_requested(vis as i32);
        }
    }

    fn apply_insert(mut self: Pin<&mut Self>, rows: Vec<ConnectionRow>) {
        // beginInsertRows must bracket exactly the range the store will grow by.
        // Derive that count from the store's own dedup-aware `count_new_ids`,
        // which mirrors `insert_rows` including the duplicate-id-in-one-batch
        // case (a naive filter against the pre-insert store over-counts there
        // and would desync the bracket from the real store delta — a
        // QAbstractListModel invariant violation).
        let existing = self.store.len();
        let appended = self.store.count_new_ids(&rows);

        if appended > 0 {
            let first = existing as i32;
            let last = (existing + appended - 1) as i32;
            unsafe {
                self.as_mut()
                    .begin_insert_rows(&QModelIndex::default(), first, last);
            }
        }
        let ops = self.as_mut().rust_mut().store.insert_rows(rows);
        // The bracketed count must equal the store's actual growth. This is the
        // snapshot-before/after reconciliation guarding the Qt invariant.
        debug_assert_eq!(
            self.store.len(),
            existing + appended,
            "beginInsertRows bracket ({appended}) desynced from actual store delta"
        );
        if appended > 0 {
            unsafe {
                self.as_mut().end_insert_rows();
            }
        }
        self.emit_changed_ops(&ops);
    }

    fn apply_remove(mut self: Pin<&mut Self>, ids: &[String]) {
        let mut indices: Vec<usize> = ids
            .iter()
            .filter_map(|id| self.store.rows().iter().position(|r| &r.id == id))
            .collect();
        if indices.is_empty() {
            return;
        }
        indices.sort_unstable();
        indices.dedup();
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for &i in &indices {
            match runs.last_mut() {
                Some(run) if run.1 == i => run.1 = i + 1,
                _ => runs.push((i, i + 1)),
            }
        }
        // High-index run first so lower indices stay valid as we remove.
        for &(start, end) in runs.iter().rev() {
            unsafe {
                self.as_mut().begin_remove_rows(
                    &QModelIndex::default(),
                    start as i32,
                    (end - 1) as i32,
                );
            }
            self.as_mut().rust_mut().store.remove_range(start, end);
            unsafe {
                self.as_mut().end_remove_rows();
            }
        }
    }

    fn apply_update(mut self: Pin<&mut Self>, rows: Vec<ConnectionRow>) {
        let ops = self.as_mut().rust_mut().store.update_rows(rows);
        self.emit_changed_ops(&ops);
    }

    fn emit_changed_ops(mut self: Pin<&mut Self>, ops: &[ModelOp]) {
        for op in ops {
            if let ModelOp::Changed { indices } = op {
                for &idx in indices {
                    let tl = self.index(idx as i32, 0, &QModelIndex::default());
                    let br = self.index(idx as i32, 0, &QModelIndex::default());
                    // Empty roles list = "all roles changed" — the view re-reads.
                    let roles = QList::<i32>::default();
                    self.as_mut().data_changed(&tl, &br, &roles);
                }
            }
        }
    }

    fn refresh_counts(mut self: Pin<&mut Self>) {
        let visible = self.store.visible_len() as i32;
        let total = self.store.len() as i32;
        let pending = self
            .store
            .rows()
            .iter()
            .filter(|r| r.action.is_none())
            .count() as i32;
        // `count` tracks what the ListView shows (post-filter); `totalCount`
        // stays the whole store so the view can distinguish empty from filtered.
        self.as_mut().set_count(visible);
        self.as_mut().set_total_count(total);
        self.as_mut().set_pending_count(pending);
    }
}
