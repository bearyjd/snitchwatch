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

use crate::connections::filter::ConnectionFilter;
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

mod level_rule_actions;

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
const ROLE_SOURCE_SESSION: i32 = 20;
/// The bridge allowed this row once because filtering was paused (#78).
const ROLE_ANSWERED_WHILE_PAUSED: i32 = 21;
/// A deferred row's verdict label (`connections::outcome::outcome_text`),
/// or a default-decided row's (E3); empty otherwise. Prompt-slot plan Part C.
const ROLE_OUTCOME_TEXT: i32 = 22;
/// When the bridge answers a pending row itself, in Unix ms; -1 if never.
/// A `real`: epoch milliseconds overflow a QML `int`.
const ROLE_ANSWER_DEADLINE_MS: i32 = 23;
/// "Make a rule…" is offered (`make_rule::offers_make_rule`): the prompt was
/// put off, or the firewall's default action decided the row (E3).
const ROLE_MAKE_RULE_OFFERED: i32 = 24;
/// Executable path of the process, empty when the daemon did not report one.
const ROLE_PROCESS_PATH: i32 = 25;
/// `"outgoing"` / `"incoming"` as the bridge reports it; empty on headers.
const ROLE_DIRECTION: i32 = 26;
/// When the connection started, in Unix ms (a `real`: ms overflow a QML
/// `int`); -1 on headers.
const ROLE_STARTED_AT_MS: i32 = 27;
/// Icon name (or path) for the process, from installed `.desktop` files;
/// the generic executable icon when none matches, empty on headers.
const ROLE_APP_ICON: i32 = 28;
/// Whether Snitchwatch answered the row silently (once-only) and it still
/// has no rule (`crate::silent_mode`); false on headers.
const ROLE_NEEDS_RULE: i32 = 29;
/// The tree tier: 0 program, 1 domain, 2 host, 3 connection (every row in
/// flat mode is a connection).
const ROLE_GROUP_LEVEL: i32 = 30;
/// The owning program's group key on host headers (what `toggleHostGroup`
/// needs besides the domain and host keys); empty elsewhere.
const ROLE_GROUP_PROCESS_KEY: i32 = 31;
/// What a tree header's rule button shows (`connections::level_rules::RuleState::token`):
/// `none`, `allow`, `deny`, `partial` or `pending`; `none` on connection rows.
const ROLE_LEVEL_RULE_STATE: i32 = 32;
/// The name of the rule a delete would remove, only while the state is
/// `allow`, `deny` or `pending`; empty otherwise.
const ROLE_LEVEL_RULE_NAME: i32 = 33;

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

        /// Recompute `oldestPendingAgeSecs` against the current wall clock.
        /// Call this from a QML `Timer` tick (elapsed time changes with no
        /// new bridge message to trigger a recompute otherwise).
        #[qinvokable]
        #[cxx_name = "refreshPendingAge"]
        fn refresh_pending_age(self: Pin<&mut ConnectionsModel>);
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Seconds since `oldest_pending_started_at_ms`, or `-1` when nothing is
/// actively pending. Qt-free and unit-tested below; the Qt-facing call site
/// just supplies `store.oldest_pending_started_at_ms(now)` and that same
/// `now`.
fn pending_age_secs(oldest_pending_started_at_ms: Option<i64>, now_ms: i64) -> i32 {
    match oldest_pending_started_at_ms {
        // Saturate rather than go negative if clock skew or a future
        // timestamp ever put `started_at_ms` ahead of `now_ms`.
        Some(started) => ((now_ms - started).max(0) / 1000) as i32,
        None => -1,
    }
}

/// Ids that arrive *pending* anywhere in a batch of buffered messages —
/// the flush-time input to the auto-select policy (Qt-free, tested below).
fn collect_new_pending_ids(msgs: &[ServerMessage]) -> Vec<String> {
    msgs.iter()
        .flat_map(|msg| match msg {
            ServerMessage::InsertConnectionRows { rows } => rows
                .iter()
                .filter(|r| is_pending(r))
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

/// QML keeps these opaque IDs in delegates and held-open dialogs. Qualifying
/// before storage makes every verdict retain its originating service session.
fn qualify_connection_ids(msg: &mut ServerMessage, connection_id: u64) {
    let qualify = |id: &mut String| *id = format!("{connection_id}:{id}");
    match msg {
        ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows } => {
            for row in rows {
                qualify(&mut row.id);
            }
        }
        ServerMessage::RemoveConnectionRows { ids } | ServerMessage::MoveConnetionRows { ids } => {
            for id in ids {
                qualify(id);
            }
        }
        _ => {}
    }
}

struct BufferedConnectionMessage {
    // None is reserved for explicit injection in headless/model tests.
    connection_id: Option<u64>,
    message: ServerMessage,
}

fn take_current_grouped_messages(
    buffered: Vec<BufferedConnectionMessage>,
    is_current: impl Fn(u64) -> bool,
) -> Vec<ServerMessage> {
    buffered
        .into_iter()
        .filter_map(|entry| {
            entry
                .connection_id
                .is_none_or(&is_current)
                .then_some(entry.message)
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
            oldest_pending_age_secs: -1,
            current_row_id: String::new(),
            source_session: None,
            pending_grouped_msgs: Vec::new(),
            grouped_flush_scheduled: false,
            level_rules: LevelRules::default(),
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
        if role == ROLE_SOURCE_SESSION {
            return QVariant::from(&QString::from(
                &self
                    .source_session
                    .map(|id| format!("{id}:"))
                    .unwrap_or_default(),
            ));
        }
        if role == ROLE_LEVEL_RULE_STATE || role == ROLE_LEVEL_RULE_NAME {
            return self.level_rule_role(index.row(), role);
        }
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
                let v = Verdict::of(row);
                QVariant::from(&QString::from(v.as_token()))
            }
            ROLE_PENDING => QVariant::from(&is_pending(row)),
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
            ROLE_MATCHED_RULE => QVariant::from(&QString::from(matched_rule_name(row))),
            ROLE_MATCHED_RULE_DISPLAY => QVariant::from(&QString::from(&matched_rule_display(row))),
            ROLE_ANSWERED_WHILE_PAUSED => QVariant::from(&answered_while_paused(row)),
            ROLE_OUTCOME_TEXT => QVariant::from(&QString::from(outcome_text(row))),
            ROLE_ANSWER_DEADLINE_MS => QVariant::from(&answer_deadline_ms(row)),
            ROLE_MAKE_RULE_OFFERED => QVariant::from(&crate::make_rule::offers_make_rule(row)),
            ROLE_PROCESS_PATH => QVariant::from(&QString::from(process_path_text(row))),
            ROLE_DIRECTION => QVariant::from(&QString::from(&row.direction)),
            ROLE_STARTED_AT_MS => QVariant::from(&started_at_ms(row)),
            ROLE_APP_ICON => QVariant::from(&QString::from(&app_icon_for_row(row))),
            ROLE_NEEDS_RULE => QVariant::from(&needs_rule_for_row(row)),
            ROLE_GROUP_LEVEL => QVariant::from(&3i32),
            ROLE_GROUP_PROCESS_KEY => QVariant::from(&QString::from("")),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_ID, QByteArray::from("rowId"));
        roles.insert(ROLE_SOURCE_SESSION, QByteArray::from("sourceSession"));
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
        roles.insert(
            ROLE_ANSWERED_WHILE_PAUSED,
            QByteArray::from("answeredWhilePaused"),
        );
        roles.insert(ROLE_OUTCOME_TEXT, QByteArray::from("outcomeText"));
        roles.insert(
            ROLE_ANSWER_DEADLINE_MS,
            QByteArray::from("answerDeadlineMs"),
        );
        roles.insert(ROLE_MAKE_RULE_OFFERED, QByteArray::from("makeRuleOffered"));
        roles.insert(ROLE_PROCESS_PATH, QByteArray::from("processPath"));
        roles.insert(ROLE_DIRECTION, QByteArray::from("direction"));
        roles.insert(ROLE_STARTED_AT_MS, QByteArray::from("startedAtMs"));
        roles.insert(ROLE_APP_ICON, QByteArray::from("appIcon"));
        roles.insert(ROLE_NEEDS_RULE, QByteArray::from("needsRule"));
        roles.insert(ROLE_GROUP_LEVEL, QByteArray::from("groupLevel"));
        roles.insert(ROLE_GROUP_PROCESS_KEY, QByteArray::from("groupProcessKey"));
        roles.insert(ROLE_LEVEL_RULE_STATE, QByteArray::from("levelRuleState"));
        roles.insert(ROLE_LEVEL_RULE_NAME, QByteArray::from("levelRuleName"));
        roles
    }

    fn load_demo_rows(self: Pin<&mut Self>) {
        if !crate::demo::enabled() {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        self.apply_server_message(ServerMessage::InsertConnectionRows {
            rows: crate::demo::demo_rows(now_ms),
        });
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
        let icons_thread = qt_thread.clone();
        crate::app_icons::when_ready(move || {
            let _ = icons_thread.queue(|qobject| qobject.refresh_app_icons());
        });
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "ConnectionsModel",
            crate::bridge_dispatch::interests_monitor,
            move |connection_id, msg, _json| {
                let mut msg = msg.clone();
                qualify_connection_ids(&mut msg, connection_id);
                let session_handles = session_handles.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if !session_handles.is_current_session(connection_id) {
                        return;
                    }
                    qobject.apply_message_for_session(msg, Some(connection_id));
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
        self.as_mut().apply_message_for_session(msg, None);
    }

    /// The rule list and rule command results go straight to
    /// [`LevelRules`]: no buffering, no reset, only the two rule roles repaint.
    fn apply_rules_message(mut self: Pin<&mut Self>, msg: &ServerMessage) {
        let applied = self.as_mut().rust_mut().level_rules.apply(msg);
        if applied.rules_changed || applied.finished.is_some() {
            self.as_mut().notify_level_rules_changed();
        }
        if let Some(finished) = applied.finished {
            self.report_level_finished(finished);
        }
    }

    /// Repaint the rule button of every header (a rule change can move any
    /// of them, and a header's state costs little to ask again).
    fn notify_level_rules_changed(self: Pin<&mut Self>) {
        let len = self.grouped_projection.len();
        if !self.grouped || len == 0 {
            return;
        }
        let no_parent = QModelIndex::default();
        let (top, bottom) = (
            self.index(0, 0, &no_parent),
            self.index(len as i32 - 1, 0, &no_parent),
        );
        let mut roles = QList::<i32>::default();
        roles.append(ROLE_LEVEL_RULE_STATE);
        roles.append(ROLE_LEVEL_RULE_NAME);
        self.data_changed(&top, &bottom, &roles);
    }

    fn report_level_finished(mut self: Pin<&mut Self>, finished: LevelFinished) {
        self.as_mut().notify_level_rules_changed();
        self.level_rule_finished(
            finished.node.level as i32,
            QString::from(&finished.node.program),
            QString::from(&finished.node.key),
            finished.ending as i32,
            QString::from(&finished.finished.status),
        );
    }

    fn level_rule_role(&self, row: i32, role: i32) -> QVariant {
        level_rule_actions::role(self, row, role)
    }

    fn apply_message_for_session(
        mut self: Pin<&mut Self>,
        msg: ServerMessage,
        connection_id: Option<u64>,
    ) {
        if matches!(
            msg,
            ServerMessage::SetRules { .. }
                | ServerMessage::UpdateRules { .. }
                | ServerMessage::RuleCommandResult { .. }
        ) {
            self.apply_rules_message(&msg);
            return;
        }
        if matches!(msg, ServerMessage::ClearConnectionRows) {
            // A new session's snapshot: no result can come for the old one.
            for finished in self.as_mut().rust_mut().level_rules.abandon_all() {
                self.as_mut().report_level_finished(finished);
            }
        }
        if self.grouped {
            if let Some(handles) = crate::bridge_runtime::handles() {
                let schedule = {
                    let mut rust = self.as_mut().rust_mut();
                    rust.pending_grouped_msgs.push(BufferedConnectionMessage {
                        connection_id,
                        message: msg,
                    });
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
        self.as_mut().rust_mut().source_session = connection_id;
        self.apply_now(msg);
    }

    /// Apply any grouped-mode messages still sitting in the debounce buffer,
    /// all inside a single Qt model reset, then re-run the auto-select policy
    /// over every pending row that arrived in the batch and re-anchor the
    /// user's selection (a reset clears the view's `currentIndex`).
    fn flush_grouped_messages(mut self: Pin<&mut Self>) {
        let buffered = {
            let mut rust = self.as_mut().rust_mut();
            rust.grouped_flush_scheduled = false;
            std::mem::take(&mut rust.pending_grouped_msgs)
        };
        // Every flush path (timer, regroup, filter, batch action) reaches this
        // guard after the final Qt queue hop. Never revive an old session.
        let handles = crate::bridge_runtime::handles();
        // All retained authenticated messages belong to the current session.
        let source_session = buffered
            .iter()
            .filter_map(|entry| entry.connection_id)
            .find(|id| {
                handles
                    .as_ref()
                    .is_some_and(|handles| handles.is_current_session(*id))
            });
        let msgs = take_current_grouped_messages(buffered, |id| {
            handles
                .as_ref()
                .is_some_and(|handles| handles.is_current_session(id))
        });
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
            rust.source_session = source_session;
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
        let projection = self.compute_grouped_projection();
        self.as_mut().rust_mut().grouped_projection = projection;
    }

    fn compute_grouped_projection(&self) -> Vec<VisibleEntry> {
        let filter = self.store.filter().clone();
        let rows_by_id: HashMap<&str, &ConnectionRow> = self
            .store
            .rows()
            .iter()
            .map(|r| (r.id.as_str(), r))
            .collect();
        self.grouping.build_host_projection(&rows_by_id, &filter)
    }

    /// Swap the grouped projection for `new` with ranged signals (one remove
    /// range, one insert range, `dataChanged` for headers whose expand flag or
    /// counts changed) instead of a model reset, so the view keeps its scroll
    /// position, selection and delegates. See `connections::projection_diff`.
    fn apply_grouped_projection(mut self: Pin<&mut Self>, new: Vec<VisibleEntry>) {
        let diff = projection_diff::diff(&self.grouped_projection, &new);
        let no_parent = QModelIndex::default();
        if !diff.removed.is_empty() {
            unsafe {
                self.as_mut().begin_remove_rows(
                    &no_parent,
                    diff.removed.start as i32,
                    (diff.removed.end - 1) as i32,
                );
            }
            self.as_mut()
                .rust_mut()
                .grouped_projection
                .drain(diff.removed.clone());
            unsafe {
                self.as_mut().end_remove_rows();
            }
        }
        if !diff.inserted.is_empty() {
            unsafe {
                self.as_mut().begin_insert_rows(
                    &no_parent,
                    diff.inserted.start as i32,
                    (diff.inserted.end - 1) as i32,
                );
            }
            self.as_mut().rust_mut().grouped_projection.splice(
                diff.inserted.start..diff.inserted.start,
                new[diff.inserted.clone()].iter().cloned(),
            );
            unsafe {
                self.as_mut().end_insert_rows();
            }
        }
        for &idx in &diff.changed {
            self.as_mut().rust_mut().grouped_projection[idx] = new[idx].clone();
            let index = self.index(idx as i32, 0, &no_parent);
            self.as_mut()
                .data_changed(&index, &index, &QList::<i32>::default());
        }
        debug_assert_eq!(self.grouped_projection, new, "ranged update desynced");
    }

    /// Replace the active filter, bracketed by a model reset (the whole visible
    /// set changes), and refresh the derived counts. Filter/search applies in
    /// both flat and grouped modes — the grouped projection is rebuilt too so
    /// matched children keep their ancestor group headers visible.
    fn apply_filter(mut self: Pin<&mut Self>, filter: ConnectionFilter) {
        // Anything still in the grouped debounce buffer must land first so the
        // filter is computed over current data.
        self.as_mut().flush_grouped_messages();
        if self.grouped {
            self.as_mut().rust_mut().store.set_filter(filter);
            let projection = self.compute_grouped_projection();
            self.as_mut().apply_grouped_projection(projection);
        } else {
            unsafe {
                self.as_mut().begin_reset_model();
            }
            self.as_mut().rust_mut().store.set_filter(filter);
            unsafe {
                self.as_mut().end_reset_model();
            }
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

    /// Parity 2: destination IP (plus byte counters, currently always 0 from
    /// the bridge and not displayed — issue #49) for a single row, for the
    /// pending-decision dialog's insight panel.
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
            #[serde(rename = "bindableProcessPath")]
            bindable_process_path: bool,
            #[serde(rename = "alsoListedByDefault")]
            also_listed_by_default: bool,
        }

        let details = RowDetails {
            dst_ip: &row.dst_ip,
            bytes_sent: row.bytes_sent,
            bytes_received: row.bytes_received,
            bindable_process_path: is_bindable_process_path(
                row.process_path.as_deref().unwrap_or_default(),
            ),
            also_listed_by_default: crate::connections::outcome::may_be_listed_again(row),
        };
        QString::from(&serde_json::to_string(&details).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: row details serialize failed");
            "{}".to_string()
        }))
    }

    fn simulation_prefill_json(&self, id: &QString) -> QString {
        let Some(row) = self.store.row_by_id(&id.to_string()) else {
            return QString::from("{}");
        };
        let form = crate::rules::simulator::SimulationForm::for_connection(row);
        QString::from(&serde_json::to_string(&form).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: simulation prefill serialize failed");
            "{}".to_string()
        }))
    }

    fn is_pending_row(&self, id: &QString) -> bool {
        self.store.is_pending(&id.to_string()).unwrap_or(false)
    }

    fn make_rule(
        &self,
        id: &QString,
        choice: &QString,
        scope: &QString,
        duration: &QString,
        request_id: &QString,
    ) -> QString {
        let id = id.to_string();
        let made_at_ms = now_ms();
        let message = self.store.row_by_id(&id).and_then(|row| {
            let (choice, scope, duration, request_id) = (
                choice.to_string(),
                scope.to_string(),
                duration.to_string(),
                request_id.to_string(),
            );
            crate::make_rule::add_rule_message(
                row,
                &choice,
                &scope,
                &duration,
                made_at_ms,
                &request_id,
            )
        });
        let problem = crate::make_rule::send_problem(message, |msg| {
            crate::bridge_feed::dispatch_for_row(&id, msg)
        });
        QString::from(problem.unwrap_or(""))
    }

    fn create_level_rule(
        self: Pin<&mut Self>,
        level: i32,
        process_key: &QString,
        key: &QString,
        choice: &QString,
        confirmed: bool,
    ) -> QString {
        level_rule_actions::create(self, level, process_key, key, choice, confirmed)
    }

    fn delete_level_rule(
        self: Pin<&mut Self>,
        level: i32,
        process_key: &QString,
        key: &QString,
    ) -> QString {
        level_rule_actions::delete(self, level, process_key, key)
    }

    fn poll_level_rules(self: Pin<&mut Self>) {
        level_rule_actions::poll(self);
    }

    fn inline_duration_for(
        &self,
        id: &QString,
        choice: &QString,
        app_bound_rules: bool,
        allow_duration: &QString,
    ) -> QString {
        let allow_duration = allow_duration.to_string();
        let token = VerdictChoice::from_token(&choice.to_string()).map_or("this_time", |choice| {
            inline_deny::duration_token_for_row(
                &self.store,
                &id.to_string(),
                choice,
                app_bound_rules,
                &allow_duration,
            )
        });
        QString::from(token)
    }

    fn alert_row_json(&self) -> QString {
        QString::from(&crate::alert_row::oldest_pending_json(&self.store))
    }

    fn inline_deny_for(&self, id: &QString, app_bound_rules: bool) -> QString {
        QString::from(InlineDeny::for_row(&self.store, &id.to_string(), app_bound_rules).token())
    }

    fn first_pending_row_id_for_process(&self, key: &QString) -> QString {
        let ids = self.grouping.pending_row_ids(&key.to_string());
        QString::from(ids.first().map_or("", String::as_str))
    }

    /// Issue #18 batch actions: pending row ids under process group `key`,
    /// JSON-encoded as `["id1","id2",...]`. Returns `"[]"` for an unknown
    /// key or serialization failure rather than erroring — the QML caller
    /// degrades to a no-op loop.
    fn pending_row_ids_for_process(mut self: Pin<&mut Self>, key: &QString) -> QString {
        self.as_mut().flush_grouped_messages();
        let ids = self.grouping.pending_row_ids(&key.to_string());
        QString::from(&serde_json::to_string(&ids).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: pending row ids serialize failed");
            "[]".to_string()
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
        // flat store's visible list. A new pending row's host starts collapsed,
        // so when its program and domain are open (issue #20: never force open
        // what the user collapsed) open just the host, with ranged signals, so
        // the row is in the projection to select.
        if self.grouped
            && self
                .as_mut()
                .rust_mut()
                .grouping
                .open_host_of_row_under_open_domain(&target)
        {
            let projection = self.compute_grouped_projection();
            self.as_mut().apply_grouped_projection(projection);
            self.as_mut().refresh_counts();
        }
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

    /// The icon index just became ready: rows drawn before then have the
    /// generic icon, so every row's `appIcon` is read again.
    fn refresh_app_icons(mut self: Pin<&mut Self>) {
        let rows = self.row_count(&QModelIndex::default());
        if rows <= 0 {
            return;
        }
        let tl = self.index(0, 0, &QModelIndex::default());
        let br = self.index(rows - 1, 0, &QModelIndex::default());
        let mut roles = QList::<i32>::default();
        roles.append(ROLE_APP_ICON);
        self.as_mut().data_changed(&tl, &br, &roles);
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
        let pending = self.store.rows().iter().filter(|r| is_pending(r)).count() as i32;
        // `totalCount` stays the whole store so the view can distinguish
        // empty from filtered.
        self.as_mut().set_count(visible);
        self.as_mut().set_total_count(total);
        self.as_mut().set_pending_count(pending);
        self.as_mut().refresh_pending_age();
    }

    fn refresh_pending_age(mut self: Pin<&mut Self>) {
        let now = now_ms();
        let age = pending_age_secs(self.store.oldest_pending_started_at_ms(now), now);
        self.as_mut().set_oldest_pending_age_secs(age);
    }
}

/// Whether a filtering pause answered the row (issue #78).
fn answered_while_paused(row: &ConnectionRow) -> bool {
    row.auto_answer == Some(AutoAnswer::FilterPaused)
}

fn answer_deadline_ms(row: &ConnectionRow) -> f64 {
    row.answer_deadline_ms.map_or(-1.0, |ms| ms as f64)
}

/// Read one role of a grouped-projection [`VisibleEntry`] into the QVariant
/// shape `data()` returns. `store` resolves leaf `Row` entries' full
/// `ConnectionRow` content (the tree only tracks ids).
/// The row's executable path, empty when the daemon reported none.
fn process_path_text(row: &ConnectionRow) -> &str {
    row.process_path.as_deref().unwrap_or("")
}

/// `startedAtMs` as a `real` (epoch ms overflow a QML `int`).
fn started_at_ms(row: &ConnectionRow) -> f64 {
    row.started_at_ms as f64
}

/// Icon for the row's process; the generic icon when the path is unknown.
fn app_icon_for_row(row: &ConnectionRow) -> String {
    crate::app_icons::icon_for_path(process_path_text(row))
}

/// Whether the GUI answered `row` silently and it is no longer waiting.
fn needs_rule_for_row(row: &ConnectionRow) -> bool {
    !is_pending(row) && crate::silent_mode::needs_rule(&row.id)
}

fn grouped_entry_data(entry: &VisibleEntry, role: i32, store: &RowStore) -> QVariant {
    // Roles every tier answers the same way, whatever its shape.
    match role {
        ROLE_GROUP_LEVEL => return QVariant::from(&entry.level()),
        ROLE_GROUP_PROCESS_KEY => {
            let key = match entry {
                VisibleEntry::HostHeader { process_key, .. } => process_key.as_str(),
                _ => "",
            };
            return QVariant::from(&QString::from(key));
        }
        _ => {}
    }
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
            ROLE_MATCHED_RULE | ROLE_MATCHED_RULE_DISPLAY | ROLE_OUTCOME_TEXT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_ANSWERED_WHILE_PAUSED | ROLE_MAKE_RULE_OFFERED => QVariant::from(&false),
            ROLE_ANSWER_DEADLINE_MS | ROLE_STARTED_AT_MS => QVariant::from(&-1.0f64),
            ROLE_PROCESS_PATH | ROLE_DIRECTION | ROLE_APP_ICON => {
                QVariant::from(&QString::from(""))
            }
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
            ROLE_MATCHED_RULE | ROLE_MATCHED_RULE_DISPLAY | ROLE_OUTCOME_TEXT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_ANSWERED_WHILE_PAUSED | ROLE_MAKE_RULE_OFFERED | ROLE_NEEDS_RULE => {
                QVariant::from(&false)
            }
            ROLE_ANSWER_DEADLINE_MS | ROLE_STARTED_AT_MS => QVariant::from(&-1.0f64),
            ROLE_PROCESS_PATH | ROLE_DIRECTION | ROLE_APP_ICON => {
                QVariant::from(&QString::from(""))
            }
            _ => QVariant::default(),
        },
        VisibleEntry::HostHeader {
            process_key,
            domain_key,
            key,
            label,
            expanded,
            counts,
        } => match role {
            ROLE_ID => QVariant::from(&QString::from(&format!(
                "host:{process_key}|{domain_key}|{key}"
            ))),
            ROLE_DEPTH => QVariant::from(&2i32),
            ROLE_GROUP_PARENT_KEY => QVariant::from(&QString::from(domain_key)),
            // Everything else reads as a domain header with the host's data.
            _ => grouped_entry_data(
                &VisibleEntry::DomainHeader {
                    process_key: process_key.clone(),
                    key: key.clone(),
                    label: label.clone(),
                    expanded: *expanded,
                    counts: *counts,
                },
                role,
                store,
            ),
        },
        VisibleEntry::Row { id, host_key, .. } => {
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
                    let v = Verdict::of(row);
                    QVariant::from(&QString::from(v.as_token()))
                }
                ROLE_PENDING => QVariant::from(&is_pending(row)),
                ROLE_DEPTH => QVariant::from(&if host_key.is_some() { 3i32 } else { 2i32 }),
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
                ROLE_MATCHED_RULE => QVariant::from(&QString::from(matched_rule_name(row))),
                ROLE_MATCHED_RULE_DISPLAY => {
                    QVariant::from(&QString::from(&matched_rule_display(row)))
                }
                ROLE_ANSWERED_WHILE_PAUSED => QVariant::from(&answered_while_paused(row)),
                ROLE_OUTCOME_TEXT => QVariant::from(&QString::from(outcome_text(row))),
                ROLE_ANSWER_DEADLINE_MS => QVariant::from(&answer_deadline_ms(row)),
                ROLE_MAKE_RULE_OFFERED => QVariant::from(&crate::make_rule::offers_make_rule(row)),
                ROLE_PROCESS_PATH => QVariant::from(&QString::from(process_path_text(row))),
                ROLE_DIRECTION => QVariant::from(&QString::from(&row.direction)),
                ROLE_STARTED_AT_MS => QVariant::from(&started_at_ms(row)),
                ROLE_APP_ICON => QVariant::from(&QString::from(&app_icon_for_row(row))),
                ROLE_NEEDS_RULE => QVariant::from(&needs_rule_for_row(row)),
                _ => QVariant::default(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_flush_discards_disconnected_and_replaced_sessions() {
        let buffer = || {
            vec![
                BufferedConnectionMessage {
                    connection_id: Some(1),
                    message: ServerMessage::ClearConnectionRows,
                },
                BufferedConnectionMessage {
                    connection_id: Some(2),
                    message: ServerMessage::InsertConnectionRows {
                        rows: vec![row("2:1", None)],
                    },
                },
            ]
        };
        assert!(take_current_grouped_messages(buffer(), |_| false).is_empty());
        let current = take_current_grouped_messages(buffer(), |id| id == 2);
        assert_eq!(current.len(), 1);
        assert!(
            matches!(&current[0], ServerMessage::InsertConnectionRows { rows } if rows[0].id == "2:1")
        );
    }

    #[test]
    fn row_identity_and_all_connection_deltas_preserve_source_session() {
        let mut insert = ServerMessage::InsertConnectionRows {
            rows: vec![row("1", None)],
        };
        qualify_connection_ids(&mut insert, 7);
        let mut store = RowStore::default();
        store.apply(insert);
        assert!(store.row_by_id("7:1").is_some());
        assert!(store.row_by_id("1").is_none());
        let mut update = ServerMessage::UpdateConnectionRows {
            rows: vec![row("1", Some("allow"))],
        };
        qualify_connection_ids(&mut update, 7);
        store.apply(update);
        assert_eq!(
            store.row_by_id("7:1").unwrap().action.as_deref(),
            Some("allow")
        );
        let mut moved = ServerMessage::MoveConnetionRows {
            ids: vec!["1".into()],
        };
        qualify_connection_ids(&mut moved, 7);
        assert!(matches!(&moved, ServerMessage::MoveConnetionRows { ids } if ids == &["7:1"]));
        let mut remove = ServerMessage::RemoveConnectionRows {
            ids: vec!["1".into()],
        };
        qualify_connection_ids(&mut remove, 7);
        store.apply(remove);
        assert!(store.row_by_id("7:1").is_none());
    }

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
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        }
    }

    #[test]
    fn pending_age_secs_is_sentinel_when_nothing_pending() {
        assert_eq!(pending_age_secs(None, 10_000), -1);
    }

    #[test]
    fn pending_age_secs_computes_elapsed_seconds() {
        assert_eq!(pending_age_secs(Some(1_000), 14_500), 13);
    }

    #[test]
    fn pending_age_secs_saturates_at_zero_for_clock_skew() {
        // started_at_ms ahead of now_ms (e.g. clock skew) must never go negative.
        assert_eq!(pending_age_secs(Some(5_000), 1_000), 0);
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

    #[test]
    fn new_row_helpers_read_the_bridge_row() {
        let mut r = row("a", None);
        assert_eq!(process_path_text(&r), "");
        r.process_path = Some("/usr/bin/curl".into());
        r.started_at_ms = 1_760_000_000_123;
        assert_eq!(process_path_text(&r), "/usr/bin/curl");
        assert_eq!(started_at_ms(&r), 1_760_000_000_123.0);
    }

    #[test]
    fn app_icon_is_generic_without_a_process_path() {
        assert_eq!(
            app_icon_for_row(&row("a", None)),
            crate::app_icons::FALLBACK_ICON
        );
    }

    #[test]
    fn a_silently_answered_row_needs_a_rule_once_it_is_decided() {
        let mut answered = row("7:silent-answered", None);
        assert!(!needs_rule_for_row(&answered), "unmarked");
        crate::silent_mode::mark_needs_rule("7:silent-answered");
        assert!(!needs_rule_for_row(&answered), "still waiting");
        answered.action = Some("allow".into());
        assert!(needs_rule_for_row(&answered));
        assert!(!needs_rule_for_row(&row("7:other", Some("allow"))));
    }

    #[test]
    fn group_level_reports_program_domain_host_connection() {
        use crate::connections::grouping::GroupCounts;
        let store = RowStore::default();
        let counts = GroupCounts::default();
        let level = |entry: &VisibleEntry| {
            grouped_entry_data(entry, ROLE_GROUP_LEVEL, &store).value::<i32>()
        };
        let program = VisibleEntry::ProcessHeader {
            key: "p".into(),
            label: "p".into(),
            expanded: true,
            counts,
        };
        let domain = VisibleEntry::DomainHeader {
            process_key: "p".into(),
            key: "d.com".into(),
            label: "d.com".into(),
            expanded: true,
            counts,
        };
        let host = VisibleEntry::HostHeader {
            process_key: "p".into(),
            domain_key: "d.com".into(),
            key: "h.d.com".into(),
            label: "h.d.com".into(),
            expanded: true,
            counts,
        };
        let conn = VisibleEntry::Row {
            id: "1".into(),
            process_key: "p".into(),
            domain_key: "d.com".into(),
            host_key: Some("h.d.com".into()),
        };
        assert_eq!(
            [&program, &domain, &host, &conn].map(level),
            [Some(0), Some(1), Some(2), Some(3)]
        );
        let process_key = |e: &VisibleEntry| {
            grouped_entry_data(e, ROLE_GROUP_PROCESS_KEY, &store).value::<QString>()
        };
        assert_eq!(process_key(&host).map(|k| k.to_string()), Some("p".into()));
        assert_eq!(process_key(&domain).map(|k| k.to_string()), Some("".into()));
    }

    #[test]
    fn new_roles_have_distinct_ids_and_names() {
        let ids = [
            ROLE_PROCESS_PATH,
            ROLE_DIRECTION,
            ROLE_STARTED_AT_MS,
            ROLE_APP_ICON,
            ROLE_NEEDS_RULE,
            ROLE_GROUP_LEVEL,
            ROLE_GROUP_PROCESS_KEY,
            ROLE_LEVEL_RULE_STATE,
            ROLE_LEVEL_RULE_NAME,
        ];
        for (i, a) in ids.iter().enumerate() {
            assert!(*a > ROLE_MAKE_RULE_OFFERED);
            assert!(!ids[i + 1..].contains(a));
        }
    }
}
