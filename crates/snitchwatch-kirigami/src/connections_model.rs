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
use std::collections::HashMap;

use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QVariant,
};

use crate::connections::filter::ConnectionFilter;
use crate::connections::grouping::{GroupTree, VisibleEntry};
use crate::connections::row_store::{matched_rule_display, ModelOp, RowStore, Verdict};
use snitchwatch_bridge::ws_messages::{ConnectionRow, ServerMessage};

// Role ids exposed to the QML delegate. Flat-mode leaf rows use 0-6; the
// grouping layer (Little-Snitch-parity Process->Domain view) adds 7+. None
// of these names collide with Qt Quick Controls built-ins on `ItemDelegate`
// (in particular, never name a role `action` — see module docs on
// `connections::grouping` / the design spec's pitfall note: a role or
// required-property named `action` silently breaks delegates).
const ROLE_ID: i32 = 0;
const ROLE_PROCESS: i32 = 1;
const ROLE_HOST: i32 = 2;
const ROLE_PORT: i32 = 3;
const ROLE_PROTOCOL: i32 = 4;
const ROLE_VERDICT: i32 = 5;
const ROLE_PENDING: i32 = 6;
const ROLE_DEPTH: i32 = 7;
const ROLE_IS_GROUP_HEADER: i32 = 8;
const ROLE_EXPANDED: i32 = 9;
const ROLE_GROUP_KEY: i32 = 10;
const ROLE_GROUP_PARENT_KEY: i32 = 11;
const ROLE_GROUP_LABEL: i32 = 12;
const ROLE_GROUP_TOTAL: i32 = 13;
const ROLE_GROUP_PENDING: i32 = 14;
const ROLE_GROUP_ALLOWED: i32 = 15;
const ROLE_GROUP_DENIED: i32 = 16;
const ROLE_GROUP_BLOCKLISTED: i32 = 17;
/// Raw matched-rule name (empty string when unknown/not applicable), used by
/// QML to drive the inspector's "Show rule" action — see
/// `ConnectionsPage.qml`'s inspector and `RulesModel::select_rule_by_name`.
const ROLE_MATCHED_RULE: i32 = 18;
/// Friendly display string for the same — see
/// `connections::row_store::matched_rule_display`'s doc comment for exactly
/// what it shows for pending / no-rule-name rows.
const ROLE_MATCHED_RULE_DISPLAY: i32 = 19;

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
        /// Whether the view is currently in grouped (Process -> Domain ->
        /// connection) mode as opposed to the flat list. Defaults to `true`
        /// per the design spec ("defaulting to grouped"). Toggle via the
        /// `setGrouped` invokable, not by writing this property directly —
        /// the toggle needs to rebracket the Qt model reset and rebuild the
        /// grouped projection.
        #[qproperty(bool, grouped)]
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

        // --- Grouping (Little-Snitch-parity Process->Domain view) ----------
        /// Switch between grouped (Process -> Domain -> connection) and flat
        /// list views. Named distinctly from the `grouped` qproperty and its
        /// auto-generated `setGrouped` setter (which only stores the value
        /// and emits the change signal) because this one also rebrackets the
        /// Qt reset and rebuilds the grouped projection; QML must call this,
        /// not assign the property.
        #[qinvokable]
        #[cxx_name = "setGroupedMode"]
        fn set_grouped_mode(self: Pin<&mut ConnectionsModel>, grouped: bool);

        /// Toggle a top-level process group's expand/collapse state.
        #[qinvokable]
        #[cxx_name = "toggleProcessGroup"]
        fn toggle_process_group(self: Pin<&mut ConnectionsModel>, key: &QString);

        /// Toggle a domain subgroup's expand/collapse state within its
        /// parent process group.
        #[qinvokable]
        #[cxx_name = "toggleDomainGroup"]
        fn toggle_domain_group(
            self: Pin<&mut ConnectionsModel>,
            process_key: &QString,
            domain_key: &QString,
        );

        /// Parity 2 (pending-decision insight panel + mini sparkline): the
        /// destination IP and cumulative byte counters for a single row by
        /// id, JSON-encoded as `{"dstIp","bytesSent","bytesReceived"}`.
        /// Returns `"{}"` for an unknown id (e.g. a group-header pseudo-row)
        /// rather than erroring — the inspector degrades to blank fields.
        #[qinvokable]
        #[cxx_name = "rowDetailsJson"]
        fn row_details_json(self: &ConnectionsModel, id: &QString) -> QString;
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

/// How long grouped-mode mutations are coalesced before one Qt model reset
/// flushes them all. Chosen well under perception threshold but long enough
/// that a busy system's per-connection event bursts (dozens/sec) collapse to
/// ~6 resets/sec worst case instead of one per message.
const GROUPED_FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

/// Ids that arrive *pending* anywhere in a batch of buffered messages —
/// the flush-time input to the auto-select policy (Qt-free, tested below).
fn collect_new_pending_ids(msgs: &[ServerMessage]) -> Vec<String> {
    msgs.iter()
        .flat_map(|msg| match msg {
            ServerMessage::InsertConnectionRows { rows } => rows
                .iter()
                .filter(|r| r.action.is_none())
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

/// Mirror one message's delta into the Process->Domain group tree, Qt-free
/// and incremental (see `connections::grouping` module docs) — shared by the
/// immediate and debounced apply paths.
fn mirror_into_grouping(rust: &mut ConnectionsModelRust, msg: &ServerMessage) {
    match msg {
        ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows } => {
            for row in rows {
                rust.grouping.upsert_row(row);
            }
        }
        ServerMessage::RemoveConnectionRows { ids } => {
            for id in ids {
                rust.grouping.remove_row(id);
            }
        }
        ServerMessage::MoveConnetionRows { ids } => {
            rust.grouping.move_to_front(ids);
        }
        ServerMessage::ClearConnectionRows => {
            rust.grouping.clear();
        }
        _ => {}
    }
}

/// Rust-side state for [`qobject::ConnectionsModel`].
pub struct ConnectionsModelRust {
    store: RowStore,
    /// Process -> Domain group tree (Little-Snitch-parity grouped view),
    /// mirrored incrementally alongside `store` from every applied
    /// `ServerMessage` — see `apply_server_message`'s grouping-mirror step.
    grouping: GroupTree,
    /// Cached flattened projection the grouped view reads from `data()` /
    /// `row_count()`. Rebuilt (not the tree itself — see `connections::
    /// grouping` module docs) after every mutation and after every
    /// expand/collapse toggle.
    grouped_projection: Vec<VisibleEntry>,
    /// Backing field for the `grouped` qproperty — see its doc comment in
    /// the `#[cxx_qt::bridge]` block. Defaults to `true` (grouped view).
    grouped: bool,
    count: i32,
    pending_count: i32,
    total_count: i32,
    /// Id of the row the user currently has selected (empty = none). Fed from
    /// QML via `setCurrentRowId`; consulted by the auto-select policy.
    current_row_id: String,
    /// Grouped-mode debounce buffer: messages received but not yet applied.
    /// Only used when the bridge runtime is available to schedule the flush;
    /// see `apply_server_message`. Never touched in flat mode.
    pending_grouped_msgs: Vec<ServerMessage>,
    /// Whether a debounced flush is already scheduled for the buffer above.
    grouped_flush_scheduled: bool,
}

impl Default for ConnectionsModelRust {
    fn default() -> Self {
        Self {
            store: RowStore::default(),
            grouping: GroupTree::default(),
            grouped_projection: Vec::new(),
            grouped: true,
            count: 0,
            pending_count: 0,
            total_count: 0,
            current_row_id: String::new(),
            pending_grouped_msgs: Vec::new(),
            grouped_flush_scheduled: false,
        }
    }
}

impl qobject::ConnectionsModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        if self.grouped {
            self.grouped_projection.len() as i32
        } else {
            self.store.visible_len() as i32
        }
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        if self.grouped {
            let Some(entry) = self.grouped_projection.get(index.row() as usize) else {
                return QVariant::default();
            };
            return grouped_entry_data(entry, role, &self.store);
        }
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
            ROLE_DEPTH => QVariant::from(&0i32),
            ROLE_IS_GROUP_HEADER => QVariant::from(&false),
            ROLE_EXPANDED => QVariant::from(&false),
            ROLE_GROUP_KEY | ROLE_GROUP_PARENT_KEY | ROLE_GROUP_LABEL => {
                QVariant::from(&QString::from(""))
            }
            ROLE_GROUP_TOTAL
            | ROLE_GROUP_PENDING
            | ROLE_GROUP_ALLOWED
            | ROLE_GROUP_DENIED
            | ROLE_GROUP_BLOCKLISTED => QVariant::from(&0i32),
            ROLE_MATCHED_RULE => {
                QVariant::from(&QString::from(row.matched_rule.as_deref().unwrap_or("")))
            }
            ROLE_MATCHED_RULE_DISPLAY => QVariant::from(&QString::from(&matched_rule_display(row))),
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
        roles.insert(ROLE_DEPTH, QByteArray::from("depth"));
        roles.insert(ROLE_IS_GROUP_HEADER, QByteArray::from("isGroupHeader"));
        roles.insert(ROLE_EXPANDED, QByteArray::from("expanded"));
        roles.insert(ROLE_GROUP_KEY, QByteArray::from("groupKey"));
        roles.insert(ROLE_GROUP_PARENT_KEY, QByteArray::from("groupParentKey"));
        roles.insert(ROLE_GROUP_LABEL, QByteArray::from("groupLabel"));
        roles.insert(ROLE_GROUP_TOTAL, QByteArray::from("groupTotal"));
        roles.insert(ROLE_GROUP_PENDING, QByteArray::from("groupPending"));
        roles.insert(ROLE_GROUP_ALLOWED, QByteArray::from("groupAllowed"));
        roles.insert(ROLE_GROUP_DENIED, QByteArray::from("groupDenied"));
        roles.insert(ROLE_GROUP_BLOCKLISTED, QByteArray::from("groupBlocklisted"));
        roles.insert(ROLE_MATCHED_RULE, QByteArray::from("matchedRule"));
        roles.insert(
            ROLE_MATCHED_RULE_DISPLAY,
            QByteArray::from("matchedRuleDisplay"),
        );
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
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "ConnectionsModel",
            crate::bridge_dispatch::interests_connections,
            move |_msg, json| {
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );
    }
}

impl qobject::ConnectionsModel {
    /// Apply a typed bridge message. In grouped mode with the bridge runtime
    /// available, messages are *buffered* and flushed in one Qt model reset
    /// per [`GROUPED_FLUSH_DEBOUNCE`] window ([`Self::flush_grouped_messages`])
    /// — a busy system's event bursts would otherwise cost one full reset
    /// (delegate churn, scroll/selection loss) per message. Everywhere else
    /// (flat mode, headless tests, bridge-less operation) the message applies
    /// immediately via [`Self::apply_now`].
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: ServerMessage) {
        if self.grouped {
            if let Some(handles) = crate::bridge_runtime::handles() {
                let schedule = {
                    let mut rust = self.as_mut().rust_mut();
                    rust.pending_grouped_msgs.push(msg);
                    if rust.grouped_flush_scheduled {
                        false
                    } else {
                        rust.grouped_flush_scheduled = true;
                        true
                    }
                };
                if schedule {
                    let qt_thread = self.qt_thread();
                    handles.runtime().spawn(async move {
                        tokio::time::sleep(GROUPED_FLUSH_DEBOUNCE).await;
                        let _ = qt_thread.queue(|qobject| qobject.flush_grouped_messages());
                    });
                }
                return;
            }
        }
        self.apply_now(msg);
    }

    /// Apply any grouped-mode messages still sitting in the debounce buffer,
    /// all inside a single Qt model reset, then re-run the auto-select policy
    /// over every pending row that arrived in the batch and re-anchor the
    /// user's selection (a reset clears the view's `currentIndex`).
    fn flush_grouped_messages(mut self: Pin<&mut Self>) {
        let msgs = {
            let mut rust = self.as_mut().rust_mut();
            rust.grouped_flush_scheduled = false;
            std::mem::take(&mut rust.pending_grouped_msgs)
        };
        if msgs.is_empty() {
            return;
        }
        let new_pending_ids = collect_new_pending_ids(&msgs);
        let selected_before = self.current_row_id.clone();

        unsafe {
            self.as_mut().begin_reset_model();
        }
        {
            let mut rust = self.as_mut().rust_mut();
            for msg in msgs {
                mirror_into_grouping(&mut rust, &msg);
                rust.store.apply(msg);
            }
            if rust.store.is_filtered() {
                rust.store.recompute_visible();
            }
        }
        self.as_mut().rebuild_grouped_projection();
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.as_mut().refresh_counts();
        self.as_mut().maybe_auto_select(&new_pending_ids);

        // If auto-select didn't move the selection, re-anchor the row the
        // user had selected before the reset (if it still exists/is visible).
        if !selected_before.is_empty() && self.current_row_id == selected_before {
            let vis = self
                .grouped_projection
                .iter()
                .position(|e| matches!(e, VisibleEntry::Row { id, .. } if id == &selected_before));
            if let Some(vis) = vis {
                self.as_mut().auto_select_requested(vis as i32);
            }
        }
    }

    /// The immediate (non-debounced) apply path: replay one message behind
    /// the correct Qt begin/end signals.
    fn apply_now(mut self: Pin<&mut Self>, msg: ServerMessage) {
        // Ids that arrive *pending* in this batch — captured before mutation so
        // the auto-select policy can react to them afterward.
        let new_pending_ids = collect_new_pending_ids(std::slice::from_ref(&msg));

        // Mirror the same delta into the Process->Domain group tree, Qt-free
        // and incremental (see `connections::grouping` module docs), so both
        // projections stay live regardless of which one is currently on
        // screen. This must happen before `msg` is consumed by `store.apply`
        // below, hence matching on `&msg` here.
        {
            let mut rust = self.as_mut().rust_mut();
            mirror_into_grouping(&mut rust, &msg);
        }

        if self.grouped {
            // The grouped view's visible-row set (headers appearing/
            // disappearing, expand state interacting with membership changes)
            // doesn't map onto incremental begin/insert/remove brackets as
            // cleanly as the flat list does; mirroring the precedent the
            // filtered-flat branch below already sets ("a filter is an
            // explicit user investigation mode... a reset is acceptable
            // here"), the grouped view is refreshed via a full Qt reset while
            // the underlying tree/aggregates stay incrementally maintained.
            unsafe {
                self.as_mut().begin_reset_model();
            }
            {
                let mut rust = self.as_mut().rust_mut();
                rust.store.apply(msg);
                if rust.store.is_filtered() {
                    rust.store.recompute_visible();
                }
            }
            self.as_mut().rebuild_grouped_projection();
            unsafe {
                self.as_mut().end_reset_model();
            }
        } else if self.store.is_filtered() {
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

    /// Rebuild the cached flattened grouped projection from the current
    /// group tree, store rows, and active filter. Cheap relative to a tree
    /// rebuild — see `connections::grouping` module docs.
    fn rebuild_grouped_projection(mut self: Pin<&mut Self>) {
        let filter = self.store.filter().clone();
        let projection = {
            let rows_by_id: HashMap<&str, &ConnectionRow> = self
                .store
                .rows()
                .iter()
                .map(|r| (r.id.as_str(), r))
                .collect();
            self.grouping.build_projection(&rows_by_id, &filter)
        };
        self.as_mut().rust_mut().grouped_projection = projection;
    }

    /// Replace the active filter, bracketed by a model reset (the whole visible
    /// set changes), and refresh the derived counts. Filter/search applies in
    /// both flat and grouped modes — the grouped projection is rebuilt too so
    /// matched children keep their ancestor group headers visible.
    fn apply_filter(mut self: Pin<&mut Self>, filter: ConnectionFilter) {
        // Anything still in the grouped debounce buffer must land first so the
        // filter is computed over current data.
        self.as_mut().flush_grouped_messages();
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().rust_mut().store.set_filter(filter);
        if self.grouped {
            self.as_mut().rebuild_grouped_projection();
        }
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.refresh_counts();
    }

    /// Switch between grouped and flat views. A no-op if already in the
    /// requested mode. Bracketed by a reset since the entire visible row set
    /// changes shape.
    fn set_grouped_mode(mut self: Pin<&mut Self>, grouped: bool) {
        if grouped == self.grouped {
            return;
        }
        // Buffered grouped-mode messages must apply before the mode flips so
        // the flat view starts from current data.
        self.as_mut().flush_grouped_messages();
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().set_grouped(grouped);
        if grouped {
            self.as_mut().rebuild_grouped_projection();
        }
        unsafe {
            self.as_mut().end_reset_model();
        }
    }

    /// Toggle a process group's expand/collapse state and refresh the
    /// grouped projection. No-op when not currently in grouped mode.
    fn toggle_process_group(mut self: Pin<&mut Self>, key: &QString) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut()
            .rust_mut()
            .grouping
            .toggle_process(&key.to_string());
        self.as_mut().rebuild_grouped_projection();
        unsafe {
            self.as_mut().end_reset_model();
        }
    }

    /// Toggle a domain subgroup's expand/collapse state within its parent
    /// process group and refresh the grouped projection. No-op when not
    /// currently in grouped mode.
    fn toggle_domain_group(mut self: Pin<&mut Self>, process_key: &QString, domain_key: &QString) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut()
            .rust_mut()
            .grouping
            .toggle_domain(&process_key.to_string(), &domain_key.to_string());
        self.as_mut().rebuild_grouped_projection();
        unsafe {
            self.as_mut().end_reset_model();
        }
    }

    /// Parity 2: destination IP + cumulative byte counters for a single row,
    /// for the pending-decision dialog's insight panel and "this connection"
    /// sparkline readout.
    fn row_details_json(&self, id: &QString) -> QString {
        let id = id.to_string();
        let Some(row) = self.store.row_by_id(&id) else {
            return QString::from("{}");
        };

        #[derive(serde::Serialize)]
        struct RowDetails<'a> {
            #[serde(rename = "dstIp")]
            dst_ip: &'a str,
            #[serde(rename = "bytesSent")]
            bytes_sent: u64,
            #[serde(rename = "bytesReceived")]
            bytes_received: u64,
        }

        let details = RowDetails {
            dst_ip: &row.dst_ip,
            bytes_sent: row.bytes_sent,
            bytes_received: row.bytes_received,
        };
        QString::from(&serde_json::to_string(&details).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: row details serialize failed");
            "{}".to_string()
        }))
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
        // target is filtered out of view there's nothing to select. In grouped
        // mode this is a position in the flattened projection rather than the
        // flat store's visible list; the projection is guaranteed to already
        // contain the row because `build_projection` force-expands the path
        // to any group with a pending descendant (see `connections::grouping`
        // module docs), which satisfies "auto-expand the path to a new
        // pending row" without any extra state here.
        let vis = if self.grouped {
            self.grouped_projection
                .iter()
                .position(|entry| matches!(entry, VisibleEntry::Row { id, .. } if id == &target))
        } else {
            self.store.visible_index_of(&target)
        };
        if let Some(vis) = vis {
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
        // `count` tracks what the ListView actually shows: the grouped
        // projection's row count (headers + visible leaves) in grouped mode,
        // the flat filtered/unfiltered visible length otherwise.
        let visible = if self.grouped {
            self.grouped_projection.len() as i32
        } else {
            self.store.visible_len() as i32
        };
        let total = self.store.len() as i32;
        let pending = self
            .store
            .rows()
            .iter()
            .filter(|r| r.action.is_none())
            .count() as i32;
        // `totalCount` stays the whole store so the view can distinguish
        // empty from filtered.
        self.as_mut().set_count(visible);
        self.as_mut().set_total_count(total);
        self.as_mut().set_pending_count(pending);
    }
}


/// Read one role of a grouped-projection [`VisibleEntry`] into the QVariant
/// shape `data()` returns. `store` resolves leaf `Row` entries' full
/// `ConnectionRow` content (the tree only tracks ids).
fn grouped_entry_data(entry: &VisibleEntry, role: i32, store: &RowStore) -> QVariant {
    match entry {
        VisibleEntry::ProcessHeader {
            key,
            label,
            expanded,
            counts,
        } => match role {
            ROLE_ID => QVariant::from(&QString::from(&format!("process:{key}"))),
            ROLE_PROCESS | ROLE_HOST | ROLE_PROTOCOL | ROLE_VERDICT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_PORT => QVariant::from(&0i32),
            ROLE_PENDING => QVariant::from(&(counts.pending > 0)),
            ROLE_DEPTH => QVariant::from(&0i32),
            ROLE_IS_GROUP_HEADER => QVariant::from(&true),
            ROLE_EXPANDED => QVariant::from(expanded),
            ROLE_GROUP_KEY => QVariant::from(&QString::from(key)),
            ROLE_GROUP_PARENT_KEY => QVariant::from(&QString::from("")),
            ROLE_GROUP_LABEL => QVariant::from(&QString::from(label)),
            ROLE_GROUP_TOTAL => QVariant::from(&(counts.total as i32)),
            ROLE_GROUP_PENDING => QVariant::from(&(counts.pending as i32)),
            ROLE_GROUP_ALLOWED => QVariant::from(&(counts.allowed as i32)),
            ROLE_GROUP_DENIED => QVariant::from(&(counts.denied as i32)),
            ROLE_GROUP_BLOCKLISTED => QVariant::from(&(counts.blocklisted as i32)),
            ROLE_MATCHED_RULE | ROLE_MATCHED_RULE_DISPLAY => QVariant::from(&QString::from("")),
            _ => QVariant::default(),
        },
        VisibleEntry::DomainHeader {
            process_key,
            key,
            label,
            expanded,
            counts,
        } => match role {
            ROLE_ID => QVariant::from(&QString::from(&format!("domain:{process_key}|{key}"))),
            ROLE_PROCESS | ROLE_HOST | ROLE_PROTOCOL | ROLE_VERDICT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_PORT => QVariant::from(&0i32),
            ROLE_PENDING => QVariant::from(&(counts.pending > 0)),
            ROLE_DEPTH => QVariant::from(&1i32),
            ROLE_IS_GROUP_HEADER => QVariant::from(&true),
            ROLE_EXPANDED => QVariant::from(expanded),
            ROLE_GROUP_KEY => QVariant::from(&QString::from(key)),
            ROLE_GROUP_PARENT_KEY => QVariant::from(&QString::from(process_key)),
            ROLE_GROUP_LABEL => QVariant::from(&QString::from(label)),
            ROLE_GROUP_TOTAL => QVariant::from(&(counts.total as i32)),
            ROLE_GROUP_PENDING => QVariant::from(&(counts.pending as i32)),
            ROLE_GROUP_ALLOWED => QVariant::from(&(counts.allowed as i32)),
            ROLE_GROUP_DENIED => QVariant::from(&(counts.denied as i32)),
            ROLE_GROUP_BLOCKLISTED => QVariant::from(&(counts.blocklisted as i32)),
            ROLE_MATCHED_RULE | ROLE_MATCHED_RULE_DISPLAY => QVariant::from(&QString::from("")),
            _ => QVariant::default(),
        },
        VisibleEntry::Row { id, .. } => {
            let Some(row) = store.row_by_id(id) else {
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
                ROLE_DEPTH => QVariant::from(&2i32),
                ROLE_IS_GROUP_HEADER => QVariant::from(&false),
                ROLE_EXPANDED => QVariant::from(&false),
                ROLE_GROUP_KEY | ROLE_GROUP_PARENT_KEY | ROLE_GROUP_LABEL => {
                    QVariant::from(&QString::from(""))
                }
                ROLE_GROUP_TOTAL
                | ROLE_GROUP_PENDING
                | ROLE_GROUP_ALLOWED
                | ROLE_GROUP_DENIED
                | ROLE_GROUP_BLOCKLISTED => QVariant::from(&0i32),
                ROLE_MATCHED_RULE => {
                    QVariant::from(&QString::from(row.matched_rule.as_deref().unwrap_or("")))
                }
                ROLE_MATCHED_RULE_DISPLAY => {
                    QVariant::from(&QString::from(&matched_rule_display(row)))
                }
                _ => QVariant::default(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "p".into(),
            process_path: None,
            dst_host: "h".into(),
            dst_ip: "1.1.1.1".into(),
            dst_port: 1,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: action.map(str::to_string),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
        }
    }

    #[test]
    fn collect_new_pending_ids_spans_the_whole_batch() {
        let msgs = vec![
            ServerMessage::InsertConnectionRows {
                rows: vec![row("a", None), row("b", Some("allow"))],
            },
            ServerMessage::ClearConnectionRows,
            ServerMessage::InsertConnectionRows {
                rows: vec![row("c", None)],
            },
            // Updates never count as *new* pending arrivals.
            ServerMessage::UpdateConnectionRows {
                rows: vec![row("d", None)],
            },
        ];
        assert_eq!(collect_new_pending_ids(&msgs), vec!["a", "c"]);
    }

    #[test]
    fn collect_new_pending_ids_empty_for_non_insert_batches() {
        assert!(collect_new_pending_ids(&[ServerMessage::ClearConnectionRows]).is_empty());
        assert!(collect_new_pending_ids(&[]).is_empty());
    }
}
