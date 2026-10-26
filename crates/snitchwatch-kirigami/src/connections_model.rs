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
//! [`qobject::ConnectionsModel::apply_server_message`]. The reconnecting
//! external-service client exposes the same typed receiver to this model.

use core::pin::Pin;
use std::collections::HashMap;

use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QVariant,
};

use crate::connections::collapse::RowCollapse;
use crate::connections::filter::{ConnectionFilter, GeoScope};
use crate::connections::grouping::{GroupTree, VisibleEntry};
use crate::connections::level_rules::{LevelFinished, LevelRules};
use crate::connections::outcome::{is_pending, outcome_text};
use crate::connections::projection_diff;
use crate::connections::row_store::{
    matched_rule_display, matched_rule_name, ModelOp, RowStore, Verdict,
};
use crate::inline_deny::{self, InlineDeny};
use crate::pending_decision::VerdictChoice;
use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;
use snitchwatch_bridge::ws_messages::{AutoAnswer, ConnectionRow, ServerMessage};

pub(crate) mod level_rule_actions;

mod apply;
mod geography;
mod helpers;
mod program_label;
mod qt_interface;
mod queries;
mod retries;
mod roles;
#[cfg(test)]
mod tests;
mod verdict_windows;

use helpers::*;
use roles::*;

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
        /// Filtered connection leaves, independent of headers and collapse.
        #[qproperty(i32, matching_count, cxx_name = "matchingCount")]
        #[qproperty(QString, geo_scope_kind, cxx_name = "geoScopeKind")]
        #[qproperty(QString, geo_scope_key, cxx_name = "geoScopeKey")]
        /// Number of rows still awaiting a decision, across the *whole* store
        /// (independent of the filter) — drives the pending badge/tray.
        #[qproperty(i32, pending_count, cxx_name = "pendingCount")]
        /// Total rows in the store regardless of filter — lets the view tell
        /// "no connections yet" apart from "no rows match the filter".
        #[qproperty(i32, total_count, cxx_name = "totalCount")]
        /// Seconds the longest-pending row has been awaiting a decision, or
        /// `-1` when nothing is pending (mirrors `remainingSeconds`'s -1
        /// sentinel convention in `PendingDecisionSheet.qml`). Drives the
        /// pending-decision-exposure warning banner: while any one `AskRule`
        /// is outstanding, opensnitchd silently defaults every *other*
        /// concurrent connection (see
        /// docs/superpowers/plans/2026-08-05-pending-decision-exposure-warning.md).
        /// Recomputed on every store mutation and on each `refreshPendingAge`
        /// call — QML must poll the latter on a timer, since elapsed time
        /// advances with no new message to trigger recomputation.
        #[qproperty(i32, oldest_pending_age_secs, cxx_name = "oldestPendingAgeSecs")]
        /// A held-back notice count is shown (`retries`): `main.qml` keeps
        /// its refresh tick running for it, so the count's hour ends too.
        #[qproperty(bool, held_back_shown, cxx_name = "heldBackShown")]
        /// Whether the view is currently in grouped (Process -> Domain ->
        /// connection) mode as opposed to the flat list. Defaults to `true`
        /// per the design spec ("defaulting to grouped"). Toggle via the
        /// `setGrouped` invokable, not by writing this property directly —
        /// the toggle needs to rebracket the Qt model reset and rebuild the
        /// grouped projection.
        #[qproperty(bool, grouped)]
        /// The search text now in force, so the search field can follow a
        /// query set from elsewhere (a map click calls `setFilterQuery`).
        #[qproperty(QString, filter_text, cxx_name = "filterText")]
        /// Whether any filter (search, pending only, denied only, from now)
        /// is narrowing the list.
        #[qproperty(bool, filter_active, cxx_name = "filterActive")]
        /// Plain-text description of the active filter, including why a term
        /// matches nothing (no GeoIP database, a bad port). Empty when no
        /// filter is active.
        #[qproperty(QString, filter_summary, cxx_name = "filterSummary")]
        /// Start of "From now" in ms since the epoch, or `0` when it is off.
        /// Session-only; never persisted.
        #[qproperty(i64, filter_since_ms, cxx_name = "filterSinceMs")]
        /// Whether "Pending only" is on. Named `filter*` so the generated
        /// property setter does not collide with the `setPendingOnly` command.
        #[qproperty(bool, filter_pending_only, cxx_name = "filterPendingOnly")]
        /// Whether "Denied only" is on (see `filterPendingOnly`).
        #[qproperty(bool, filter_denied_only, cxx_name = "filterDeniedOnly")]
        type ConnectionsModel = super::ConnectionsModelRust;

        /// Emitted when the auto-select policy decides a freshly arrived pending
        /// row should take the selection. `row` is the *visible* index to
        /// select. A signal (not a property) so it always fires — even when the
        /// target index equals the current one — and never steals focus except
        /// when the policy says to. QML connects it to `ListView.currentIndex`.
        #[qsignal]
        #[cxx_name = "autoSelectRequested"]
        fn auto_select_requested(self: Pin<&mut ConnectionsModel>, row: i32);

        /// A rule button's create or delete ended. `level` is 0 program, 1
        /// domain, 2 host; `ending` is `make_rule::Ending` (0 failed, 1 done,
        /// 2 unknown); `status` is plain text from the bridge's result.
        #[qsignal]
        #[cxx_name = "levelRuleFinished"]
        fn level_rule_finished(
            self: Pin<&mut ConnectionsModel>,
            level: i32,
            process_key: QString,
            key: QString,
            ending: i32,
            status: QString,
        );

        #[qsignal]
        #[cxx_name = "geographicSelectionRequested"]
        fn geographic_selection_requested(self: Pin<&mut ConnectionsModel>, id: QString);

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
        /// Set the search query: whitespace-separated terms, all of which must
        /// match. `app:`, `host:`, `port:` and `country:` (ISO-A2) narrow one
        /// field; anything else is a case-insensitive substring. Empty clears.
        #[qinvokable]
        #[cxx_name = "setFilterQuery"]
        fn set_filter_query(self: Pin<&mut ConnectionsModel>, query: &QString);

        /// Apply an exact geographic scope AND existing search. Empty clears
        /// geography only; invalid input retains the previous scope.
        #[qinvokable]
        #[cxx_name = "setGeoScope"]
        fn set_geo_scope(self: Pin<&mut ConnectionsModel>, kind: &QString, key: &QString) -> bool;

        /// Stable IDs under the selected visible row/app/domain/host, with
        /// existing search and geographic scope applied.
        #[qinvokable]
        #[cxx_name = "destinationRowIdsJson"]
        fn destination_row_ids_json(self: &ConnectionsModel, index: i32) -> QString;

        /// Toggle "show only pending rows".
        #[qinvokable]
        #[cxx_name = "setPendingOnly"]
        fn set_pending_only(self: Pin<&mut ConnectionsModel>, pending_only: bool);

        /// Toggle "show only denied rows".
        #[qinvokable]
        #[cxx_name = "setDeniedOnly"]
        fn set_denied_only(self: Pin<&mut ConnectionsModel>, denied_only: bool);

        /// Turn "From now" on (hide rows that started before this moment) or
        /// off. Turning it on while already on keeps the original start.
        #[qinvokable]
        #[cxx_name = "setFromNow"]
        fn set_from_now(self: Pin<&mut ConnectionsModel>, from_now: bool);

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

        /// Fill the model with fake rows. A no-op unless the process was
        /// started with `SNITCHWATCH_DEMO=1` (`crate::demo`).
        #[qinvokable]
        #[cxx_name = "loadDemoRows"]
        fn load_demo_rows(self: Pin<&mut ConnectionsModel>);

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

        /// Toggle a host's expand/collapse state within its domain, which is
        /// within its program. Ranged signals only, like the other toggles.
        #[qinvokable]
        #[cxx_name = "toggleHostGroup"]
        fn toggle_host_group(
            self: Pin<&mut ConnectionsModel>,
            process_key: &QString,
            domain_key: &QString,
            host_key: &QString,
        );

        /// Parity 2 (pending-decision insight panel): the destination IP
        /// and byte counters for a single row by id, JSON-encoded as
        /// `{"dstIp","bytesSent","bytesReceived","bindableProcessPath"}`.
        /// `bindableProcessPath` (issue #44) is the bridge's own rule for
        /// whether an answer can be remembered for this program. The shell only reads
        /// `dstIp`: the bridge hardcodes the byte counters to 0 and opensnitchd
        /// has no per-connection counters, so QML shows no byte readout or
        /// sparkline (issue #49). The fields stay in the JSON for a future
        /// real byte source.
        /// Returns `"{}"` for an unknown id (e.g. a group-header pseudo-row)
        /// rather than erroring — the inspector degrades to blank fields.
        #[qinvokable]
        #[cxx_name = "rowDetailsJson"]
        fn row_details_json(self: &ConnectionsModel, id: &QString) -> QString;

        /// Effective operand scope from the same resolver used by createLevelRule.
        #[qinvokable]
        #[cxx_name = "levelRuleScopeText"]
        fn level_rule_scope_text(
            self: &ConnectionsModel,
            level: i32,
            program: &QString,
            key: &QString,
        ) -> QString;

        /// "Simulate this connection": the Simulate sheet's fields for row
        /// `id`, JSON-encoded (`rules::simulator::SimulationForm`). Only what
        /// the row carries is set; every other field is a blank string, which
        /// the simulator reads as unknown. `"{}"` for an unknown id.
        #[qinvokable]
        #[cxx_name = "simulationPrefillJson"]
        fn simulation_prefill_json(self: &ConnectionsModel, id: &QString) -> QString;

        /// "Make a rule…" for row `id` that `make_rule::offers_make_rule`
        /// accepts (put off, or decided by the default action): sends the
        /// rule `crate::make_rule` builds from the sheet's tokens to the
        /// row's bridge session, asking for its result under `request_id`
        /// (`MakeRuleController.begin`). Empty when it was queued (which only
        /// means sent: the result says whether the rule was created);
        /// otherwise why not, as plain text (`make_rule::send_problem`).
        #[qinvokable]
        #[cxx_name = "makeRule"]
        fn make_rule(
            self: &ConnectionsModel,
            id: &QString,
            choice: &QString,
            scope: &QString,
            duration: &QString,
            request_id: &QString,
        ) -> QString;

        /// A tree row's rule button: make an allow/deny rule at `level` (0
        /// program, 1 domain, 2 host) for the program `process_key` and, below
        /// it, the domain or host `key`. Lasts the Settings default duration.
        /// A program-wide rule needs `confirmed`. Empty when it was queued
        /// (only sent: `levelRuleFinished` says how it ended); otherwise why
        /// not, as plain text.
        #[qinvokable]
        #[cxx_name = "createLevelRule"]
        fn create_level_rule(
            self: Pin<&mut ConnectionsModel>,
            level: i32,
            process_key: &QString,
            key: &QString,
            choice: &QString,
            confirmed: bool,
            shown_duration: &QString,
        ) -> QString;

        /// Delete the one rule the node's `levelRuleState` (`allow` or
        /// `deny`) stands for. Empty when queued, else why not.
        #[qinvokable]
        #[cxx_name = "deleteLevelRule"]
        fn delete_level_rule(
            self: Pin<&mut ConnectionsModel>,
            level: i32,
            process_key: &QString,
            key: &QString,
        ) -> QString;

        /// A menu's captured session must survive through final queue admission.
        /// Program-wide creation requires its nonempty reviewed duration token.
        #[qinvokable]
        #[cxx_name = "createLevelRuleForSession"]
        fn create_level_rule_for_session(
            self: Pin<&mut ConnectionsModel>,
            level: i32,
            process_key: &QString,
            key: &QString,
            choice: &QString,
            shown_duration: &QString,
            source_session: &QString,
        ) -> QString;

        /// Delete only the exact rule name and state shown in the captured menu.
        #[qinvokable]
        #[cxx_name = "deleteLevelRuleForSession"]
        fn delete_level_rule_for_session(
            self: Pin<&mut ConnectionsModel>,
            level: i32,
            process_key: &QString,
            key: &QString,
            source_session: &QString,
            rule_name: &QString,
            rule_state: &QString,
        ) -> QString;

        /// The Settings default duration token (`this_time`, `for_5_minutes`,
        /// `until_restart`, `forever`) a rule button's rule would take now:
        /// the one `createLevelRule` sends.
        #[qinvokable]
        #[cxx_name = "levelRuleDuration"]
        fn level_rule_duration(self: &ConnectionsModel) -> QString;

        /// Settings changed: read the default duration again and repaint the
        /// rule buttons if it differs.
        #[qinvokable]
        #[cxx_name = "refreshLevelRuleDuration"]
        fn refresh_level_rule_duration(self: Pin<&mut ConnectionsModel>);

        /// Give up on rule button commands with no answer in time (called by
        /// a QML timer); `levelRuleFinished` says the outcome is unknown.
        #[qinvokable]
        #[cxx_name = "pollLevelRules"]
        fn poll_level_rules(self: Pin<&mut ConnectionsModel>);

        /// Whether `id` names a row that is still awaiting a decision: present
        /// in the store (independent of the active filter) with no action
        /// yet. `false` for an unknown id. The inspector re-checks this when
        /// rows go away so it stops offering a verdict for a prompt the
        /// bridge has withdrawn (issue #49). Read-only and never flushes the
        /// grouped debounce buffer — it runs inside the model's own
        /// `rowsRemoved`/`modelReset` emissions, where a flush would nest a
        /// second reset.
        #[qinvokable]
        #[cxx_name = "isPendingRow"]
        fn is_pending_row(self: &ConnectionsModel, id: &QString) -> bool;

        /// Session of the current snapshot, independent of filtering and delegates.
        /// Read-only: safe from modelReset callbacks, without a nested flush.
        #[qinvokable]
        #[cxx_name = "currentSourceSession"]
        fn current_source_session(self: &ConnectionsModel) -> QString;

        /// Where the connection `id` sits in the list as it is drawn now (after
        /// the filter and, in grouped mode, group expansion), or -1 if it is not
        /// a row of it. The side panel clears its selection on -1 and re-points
        /// the list's current index after a reset. Read-only and never
        /// flushing, like `isPendingRow`.
        #[qinvokable]
        #[cxx_name = "visibleRowIndex"]
        fn visible_row_index(self: &ConnectionsModel, id: &QString) -> i32;

        /// The id of the connection row at list index `index` ("" for a group
        /// header or an index out of range). `autoSelectRequested` gives an
        /// index that later inserts shift, so the page keeps the id instead.
        #[qinvokable]
        #[cxx_name = "rowIdAt"]
        fn row_id_at(self: &ConnectionsModel, index: i32) -> QString;

        /// The list index of the oldest pending connection row that is drawn
        /// now, other than the ids in `except_ids_json` (a JSON array of row
        /// ids: every row the user dismissed, issue #146; anything else
        /// excepts nothing), or -1 if none. On equal starts the row earlier
        /// in the store wins, as in `alertRowJson`. The panel selects it when
        /// it is dismissed, so a prompt that arrived while it was busy is not
        /// left unseen. Read-only and never flushing.
        #[qinvokable]
        #[cxx_name = "oldestPendingRowIndex"]
        fn oldest_pending_row_index(self: &ConnectionsModel, except_ids_json: &QString) -> i32;

        /// Inline buttons (`crate::inline_deny`; all read-only, never
        /// flushing, like `isPendingRow`, since bindings call them).
        /// `app_bound_rules` is `BridgeFeed.appBoundRulesFor(id)`.
        ///
        /// The duration token `choice` (`"allow"`/`"deny"`) sends for row
        /// `id`: a Deny sends `"until_restart"` or `"this_time"`; an Allow sends
        /// `allow_duration` (the Settings default) where a rule can be bound,
        /// else `"this_time"` (also for an unknown choice).
        #[qinvokable]
        #[cxx_name = "inlineDurationFor"]
        fn inline_duration_for(
            self: &ConnectionsModel,
            id: &QString,
            choice: &QString,
            app_bound_rules: bool,
            allow_duration: &QString,
        ) -> QString;

        /// What a Deny does for row `id`: `"until_restart"`,
        /// `"program_unknown"` or `"bridge_too_old"`.
        #[qinvokable]
        #[cxx_name = "inlineDenyFor"]
        fn inline_deny_for(self: &ConnectionsModel, id: &QString, app_bound_rules: bool)
            -> QString;

        /// The oldest row still pending, as the JSON `crate::alert_row`
        /// builds for the standalone alert (`"{}"` when none waits).
        /// Read-only, like `isPendingRow`.
        #[qinvokable]
        #[cxx_name = "alertRowJson"]
        fn alert_row_json(self: &ConnectionsModel) -> QString;

        /// The first row still pending under process group `key` (what its
        /// "Deny all" tooltip describes), or `""`.
        #[qinvokable]
        #[cxx_name = "firstPendingRowIdForProcess"]
        fn first_pending_row_id_for_process(self: &ConnectionsModel, key: &QString) -> QString;

        /// Issue #18 batch actions: the ids of every row still pending a
        /// decision under process group `key`, JSON-encoded as a plain
        /// array of strings in display order (e.g. `["r1","r2"]`). QML's
        /// "Allow all"/"Deny all" process-header buttons parse this and
        /// loop `BridgeFeed.submitVerdict` per id. Flushes any buffered
        /// grouped-mode messages first so the answer reflects current data
        /// (mirrors `toggleProcessGroup`'s flush-before-read pattern).
        #[qinvokable]
        #[cxx_name = "pendingRowIdsForProcess"]
        fn pending_row_ids_for_process(self: Pin<&mut ConnectionsModel>, key: &QString) -> QString;

        /// Pending descendants of this program/domain/host, including hidden rows.
        /// Flushes grouped messages before resolving the selected subtree.
        #[qinvokable]
        #[cxx_name = "pendingRowIdsForGroup"]
        fn pending_row_ids_for_group(
            self: Pin<&mut ConnectionsModel>,
            program: &QString,
            level: i32,
            key: &QString,
            parent_domain: &QString,
        ) -> QString;

        /// Recompute `oldestPendingAgeSecs` (and the `retriedCount` role)
        /// against the current clock. Call this from a QML `Timer` tick
        /// (elapsed time changes with no new bridge message to trigger a
        /// recompute otherwise).
        #[qinvokable]
        #[cxx_name = "refreshPendingAge"]
        fn refresh_pending_age(self: Pin<&mut ConnectionsModel>);

        /// Test seam (`connections_model::retries`): a held-back count.
        #[qinvokable]
        #[cxx_name = "holdBackNoticesForTests"]
        fn hold_back_notices_for_tests(
            self: Pin<&mut ConnectionsModel>,
            program: &QString,
            host: &QString,
            count: i32,
            window_secs: i32,
        );

        /// Roll the headers' "last hour" / "today" counts to the wall clock
        /// (60 s QML `Timer`; `connections_model::verdict_windows`).
        #[qinvokable]
        #[cxx_name = "refreshVerdictWindows"]
        fn refresh_verdict_windows(self: Pin<&mut ConnectionsModel>);
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
pub struct ConnectionsModelRust {
    store: RowStore,
    /// Repeated event rows shown as one (issue #55), in front of `store`
    /// and `grouping`.
    collapse: RowCollapse,
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
    matching_count: i32,
    geo_scope_kind: QString,
    geo_scope_key: QString,
    // Only explicit JSON/demo injection uses synchronous fixture preparation.
    // The live bridge feed always supplies off-thread prepared facts.
    fixture_geo_preparer: Option<crate::geo::prepared::GeoMessagePreparer>,
    pending_count: i32,
    total_count: i32,
    /// Backing field for the `oldestPendingAgeSecs` qproperty. See its doc
    /// comment in the `#[cxx_qt::bridge]` block.
    oldest_pending_age_secs: i32,
    /// Id of the row the user currently has selected (empty = none). Fed from
    /// QML via `setCurrentRowId`; consulted by the auto-select policy.
    current_row_id: String,
    source_session: Option<u64>,
    /// Grouped-mode debounce buffer: messages received but not yet applied.
    /// Only used when the bridge runtime is available to schedule the flush;
    /// see `apply_server_message`. Never touched in flat mode.
    pending_grouped_msgs: Vec<BufferedConnectionMessage>,
    /// Whether a debounced flush is already scheduled for the buffer above.
    grouped_flush_scheduled: bool,
    /// The firewall's rules and the rule button commands in flight, behind
    /// the `levelRuleState` and `levelRuleName` roles.
    level_rules: LevelRules,
    /// Backing fields for the `filter*` qproperties.
    filter_text: QString,
    filter_active: bool,
    filter_summary: QString,
    filter_since_ms: i64,
    filter_pending_only: bool,
    filter_denied_only: bool,
    /// The Settings default duration the rule roles are computed with.
    duration: level_rule_actions::DurationCache,
    /// Behind the `retriedCount` role (`retries`), and its test seam.
    held_back_rows: HashMap<String, u32>,
    held_back_shown: bool,
    held_back_for_tests: Option<crate::bridge_runtime::HeldBackNotices>,
}

impl Default for ConnectionsModelRust {
    fn default() -> Self {
        Self {
            store: RowStore::with_filter(
                ConnectionFilter::default()
                    .with_geo(crate::geo::shared_resolver())
                    .prepared_only(),
            ),
            collapse: RowCollapse::default(),
            grouping: GroupTree::default(),
            grouped_projection: Vec::new(),
            grouped: true,
            count: 0,
            matching_count: 0,
            geo_scope_kind: QString::default(),
            geo_scope_key: QString::default(),
            fixture_geo_preparer: None,
            pending_count: 0,
            total_count: 0,
            oldest_pending_age_secs: -1,
            current_row_id: String::new(),
            source_session: None,
            pending_grouped_msgs: Vec::new(),
            grouped_flush_scheduled: false,
            level_rules: LevelRules::default(),
            filter_text: QString::default(),
            filter_active: false,
            filter_summary: QString::default(),
            filter_since_ms: 0,
            filter_pending_only: false,
            filter_denied_only: false,
            duration: level_rule_actions::DurationCache::default(),
            held_back_rows: HashMap::new(),
            held_back_shown: false,
            held_back_for_tests: None,
        }
    }
}

impl qobject::ConnectionsModel {
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
        self.as_mut()
            .rust_mut()
            .grouping
            .toggle_process(&key.to_string());
        let projection = self.compute_grouped_projection();
        self.as_mut().apply_grouped_projection(projection);
        self.refresh_counts();
    }

    /// Toggle a domain subgroup's expand/collapse state within its parent
    /// process group and refresh the grouped projection. No-op when not
    /// currently in grouped mode.
    fn toggle_domain_group(mut self: Pin<&mut Self>, process_key: &QString, domain_key: &QString) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        self.as_mut()
            .rust_mut()
            .grouping
            .toggle_domain(&process_key.to_string(), &domain_key.to_string());
        let projection = self.compute_grouped_projection();
        self.as_mut().apply_grouped_projection(projection);
        self.refresh_counts();
    }

    /// Toggle a host's expand/collapse state and refresh the grouped
    /// projection. No-op when not currently in grouped mode.
    fn toggle_host_group(
        mut self: Pin<&mut Self>,
        process_key: &QString,
        domain_key: &QString,
        host_key: &QString,
    ) {
        if !self.grouped {
            return;
        }
        self.as_mut().flush_grouped_messages();
        self.as_mut().rust_mut().grouping.toggle_host(
            &process_key.to_string(),
            &domain_key.to_string(),
            &host_key.to_string(),
        );
        let projection = self.compute_grouped_projection();
        self.as_mut().apply_grouped_projection(projection);
        self.refresh_counts();
    }
}
