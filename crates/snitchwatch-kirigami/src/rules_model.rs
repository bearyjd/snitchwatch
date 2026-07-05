//! `RulesModel` — the `QAbstractListModel` backing the Rules tab (Task 10).
//!
//! The pure fold logic lives in [`crate::rules::row_store`] and is
//! unit-tested without Qt. This is the thin cxx-qt wrapper:
//!   * exposes the flat rule list (roles below) to `RulesPage.qml`,
//!   * `toggleEnabled(name)` / `deleteRule(name)` are `qinvokable`s that emit
//!     the bridge's typed `ClientMessage` (JSON) for the live feed to
//!     forward — mirroring `BlocklistsModel::subscribe`/`unsubscribe` and
//!     `PendingDecision::submit`'s "emit signal, no local mutation, wait for
//!     the server round-trip" pattern. No bridge changes.
//!
//! Rule list updates are low-frequency whole-list replaces/upserts (same
//! reasoning as `BlocklistsModel`), so this wrapper brackets every applied
//! change with `beginResetModel`/`endResetModel`.

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};
use tokio::sync::broadcast;

use crate::rules::row_store::{RuleSource, RulesStore};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

// Roles.
const ROLE_NAME: i32 = 0;
const ROLE_ENABLED: i32 = 1;
const ROLE_ACTION: i32 = 2;
const ROLE_DURATION: i32 = 3;
const ROLE_OPERATOR_SUMMARY: i32 = 4;
const ROLE_PRECEDENCE: i32 = 5;
const ROLE_SOURCE: i32 = 6;
const ROLE_BLOCKLIST_ID: i32 = 7;

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
        /// Flat rule list, bound by `RulesPage.qml`.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        type RulesModel = super::RulesModelRust;

        /// Emitted with a JSON-encoded `ClientMessage` (`UpdateRule` /
        /// `DeleteRule`) for the live bridge feed to forward.
        #[qsignal]
        #[cxx_name = "ruleChangeRequested"]
        fn rule_change_requested(self: Pin<&mut RulesModel>, json: QString);

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &RulesModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &RulesModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &RulesModel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut RulesModel>, json: &QString);

        /// Start the live outbound feed (Task 13): subscribe to the bridge's
        /// `ServerMessage` broadcast and queue rule-list messages onto the Qt
        /// thread. No-op when the bridge isn't running. Called from QML
        /// `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut RulesModel>);

        /// Toggle a rule's enabled flag (emits `UpdateRule` with the rule's
        /// full payload, `enabled` flipped, every other field preserved).
        #[qinvokable]
        #[cxx_name = "toggleEnabled"]
        fn toggle_enabled(self: Pin<&mut RulesModel>, name: &QString);

        /// Delete a rule by name (emits `DeleteRule`).
        #[qinvokable]
        #[cxx_name = "deleteRule"]
        fn delete_rule(self: Pin<&mut RulesModel>, name: &QString);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut RulesModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut RulesModel>);
    }

    impl cxx_qt::Threading for RulesModel {}
}

/// Rust-side state for [`qobject::RulesModel`].
#[derive(Default)]
pub struct RulesModelRust {
    store: RulesStore,
    count: i32,
}

impl qobject::RulesModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.store.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = index.row() as usize;
        let Some(rule) = self.store.row(row) else {
            return QVariant::default();
        };
        match role {
            ROLE_NAME => QVariant::from(&QString::from(&rule.name)),
            ROLE_ENABLED => QVariant::from(&rule.enabled),
            ROLE_ACTION => QVariant::from(&QString::from(rule.normalized_action())),
            ROLE_DURATION => QVariant::from(&QString::from(&rule.duration)),
            ROLE_OPERATOR_SUMMARY => QVariant::from(&QString::from(&rule.operator_summary())),
            ROLE_PRECEDENCE => QVariant::from(&(row as i32)),
            ROLE_SOURCE => {
                let source = match rule.source() {
                    RuleSource::User => "user",
                    RuleSource::Blocklist { .. } => "blocklist",
                };
                QVariant::from(&QString::from(source))
            }
            ROLE_BLOCKLIST_ID => {
                let id = match rule.source() {
                    RuleSource::User => String::new(),
                    RuleSource::Blocklist { list_id } => list_id,
                };
                QVariant::from(&QString::from(&id))
            }
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_NAME, QByteArray::from("name"));
        roles.insert(ROLE_ENABLED, QByteArray::from("enabled"));
        // Named `ruleAction` (not `action`) because `Controls.ItemDelegate`
        // (an `AbstractButton` subclass) already declares a built-in `action`
        // property (for binding a `QQuickAction`) — reusing that name here
        // silently breaks delegate component creation with no diagnostic.
        roles.insert(ROLE_ACTION, QByteArray::from("ruleAction"));
        roles.insert(ROLE_DURATION, QByteArray::from("duration"));
        roles.insert(ROLE_OPERATOR_SUMMARY, QByteArray::from("operatorSummary"));
        roles.insert(ROLE_PRECEDENCE, QByteArray::from("precedence"));
        roles.insert(ROLE_SOURCE, QByteArray::from("source"));
        roles.insert(ROLE_BLOCKLIST_ID, QByteArray::from("blocklistId"));
        roles
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "RulesModel: bad ServerMessage JSON"),
        }
    }

    fn toggle_enabled(self: Pin<&mut Self>, name: &QString) {
        let name = name.to_string();
        match self.store.toggled_rule_json(&name) {
            Some(rule) => self.emit_client(ClientMessage::UpdateRule {
                rule_id: name,
                rule,
            }),
            None => tracing::warn!(%name, "RulesModel: toggleEnabled for unknown rule, ignored"),
        }
    }

    fn delete_rule(self: Pin<&mut Self>, name: &QString) {
        self.emit_client(ClientMessage::DeleteRule {
            rule_id: name.to_string(),
        });
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("RulesModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let mut rx = handles.subscribe();
        handles.runtime().spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if !crate::bridge_dispatch::interests_rules(&msg) {
                            continue;
                        }
                        match crate::bridge_dispatch::encode_server(&msg) {
                            Ok(json) => {
                                let _ = qt_thread.queue(move |qobject| {
                                    qobject.apply_server_message_json(&QString::from(&json));
                                });
                            }
                            Err(e) => tracing::warn!(error = %e, "RulesModel feed: encode failed"),
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "RulesModel feed lagged behind bridge")
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

impl qobject::RulesModel {
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
            Ok(json) => self.as_mut().rule_change_requested(QString::from(&json)),
            Err(e) => tracing::error!(error = %e, "RulesModel: client message serialize failed"),
        }
    }
}
