//! `ProfilesModel` — the `QAbstractListModel` backing the Profiles tab.
//!
//! The pure fold logic lives in [`crate::profiles::row_store`] and is
//! unit-tested without Qt. This is the thin cxx-qt wrapper:
//!   * exposes the flat profile list (roles below) to `ProfilesPage.qml`,
//!   * `createProfile`/`renameProfile`/`updateMatchers`/`deleteProfile`/
//!     `activateProfile`/`deactivateProfile` are `qinvokable`s that emit the
//!     bridge's typed `ClientMessage` (JSON) for the live feed to forward —
//!     mirroring `BlocklistsModel::subscribe`/`unsubscribe` and
//!     `RulesModel::toggleEnabled`'s "emit signal, no local mutation, wait
//!     for the server round-trip" pattern. No bridge changes.
//!
//! Profile list updates are low-frequency whole-list replaces/upserts (same
//! reasoning as `BlocklistsModel`/`RulesModel`), so this wrapper brackets
//! every applied change with `beginResetModel`/`endResetModel`.
//!
//! Network matchers are edited as a single comma-separated `QString` (the
//! "simple string list editor" the design calls for) via
//! [`crate::profiles::parse_matchers`]/a `", "`-joined display role, rather
//! than exposing a nested list model — the matcher lists are short and this
//! keeps the QML side to a single `TextField`.

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

use crate::profiles::row_store::ProfilesStore;
use crate::profiles::{derive_profile_id, parse_matchers};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

// Roles.
const ROLE_ID: i32 = 0;
const ROLE_NAME: i32 = 1;
const ROLE_MATCHERS: i32 = 2;
const ROLE_ACTIVE: i32 = 3;

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
        /// Flat profile list, bound by `ProfilesPage.qml`.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        type ProfilesModel = super::ProfilesModelRust;

        /// Emitted with a JSON-encoded `ClientMessage` (`CreateProfile` /
        /// `UpdateProfile` / `DeleteProfile` / `ActivateProfile` /
        /// `DeactivateProfile`) for the live bridge feed to forward.
        #[qsignal]
        #[cxx_name = "profileChangeRequested"]
        fn profile_change_requested(self: Pin<&mut ProfilesModel>, json: QString);

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &ProfilesModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &ProfilesModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &ProfilesModel) -> QHash_i32_QByteArray;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut ProfilesModel>, json: &QString);

        /// Start the live outbound feed: subscribe to the bridge's
        /// `ServerMessage` broadcast and queue profile-list messages onto the
        /// Qt thread. No-op when the bridge isn't running. Called from QML
        /// `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut ProfilesModel>);

        /// Create a new profile named `name` with matchers parsed from the
        /// comma-separated `matchers_csv` (emits `CreateProfile`; the id is
        /// derived from `name`, disambiguated against the currently known
        /// profile ids).
        #[qinvokable]
        #[cxx_name = "createProfile"]
        fn create_profile(self: Pin<&mut ProfilesModel>, name: &QString, matchers_csv: &QString);

        /// Rename an existing profile, keeping its matchers unchanged (emits
        /// `UpdateProfile`).
        #[qinvokable]
        #[cxx_name = "renameProfile"]
        fn rename_profile(self: Pin<&mut ProfilesModel>, id: &QString, name: &QString);

        /// Replace an existing profile's network matchers, parsed from the
        /// comma-separated `matchers_csv`, keeping its name unchanged (emits
        /// `UpdateProfile`).
        #[qinvokable]
        #[cxx_name = "updateMatchers"]
        fn update_matchers(self: Pin<&mut ProfilesModel>, id: &QString, matchers_csv: &QString);

        /// Delete a profile by id (emits `DeleteProfile`).
        #[qinvokable]
        #[cxx_name = "deleteProfile"]
        fn delete_profile(self: Pin<&mut ProfilesModel>, id: &QString);

        /// Manually activate a profile by id (emits `ActivateProfile`).
        #[qinvokable]
        #[cxx_name = "activateProfile"]
        fn activate_profile(self: Pin<&mut ProfilesModel>, id: &QString);

        /// Manually deactivate whatever profile is active (emits
        /// `DeactivateProfile`).
        #[qinvokable]
        #[cxx_name = "deactivateProfile"]
        fn deactivate_profile(self: Pin<&mut ProfilesModel>);
    }

    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut ProfilesModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut ProfilesModel>);
    }

    impl cxx_qt::Threading for ProfilesModel {}
}

/// Rust-side state for [`qobject::ProfilesModel`].
#[derive(Default)]
pub struct ProfilesModelRust {
    store: ProfilesStore,
    count: i32,
}

impl qobject::ProfilesModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.store.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = index.row() as usize;
        let Some(profile) = self.store.row(row) else {
            return QVariant::default();
        };
        match role {
            ROLE_ID => QVariant::from(&QString::from(&profile.id)),
            ROLE_NAME => QVariant::from(&QString::from(&profile.name)),
            ROLE_MATCHERS => QVariant::from(&QString::from(&profile.network_matchers.join(", "))),
            ROLE_ACTIVE => QVariant::from(&profile.active),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_ID, QByteArray::from("profileId"));
        roles.insert(ROLE_NAME, QByteArray::from("name"));
        roles.insert(ROLE_MATCHERS, QByteArray::from("networkMatchers"));
        // Named `isActive` (not `active`) because `Controls.ItemDelegate` (an
        // `AbstractButton` subclass) already declares built-in properties;
        // `active`/`action` collisions with QtQuick.Controls internals have
        // previously broken delegate creation silently (see `RulesModel`'s
        // matching comment for `action`) — avoid the same class of name here.
        roles.insert(ROLE_ACTIVE, QByteArray::from("isActive"));
        roles
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "ProfilesModel: bad ServerMessage JSON"),
        }
    }

    fn create_profile(self: Pin<&mut Self>, name: &QString, matchers_csv: &QString) {
        let name = name.to_string();
        let existing_ids = self.store.ids();
        let id = derive_profile_id(&name, &existing_ids);
        let network_matchers = parse_matchers(&matchers_csv.to_string());
        self.emit_client(ClientMessage::CreateProfile {
            id,
            name,
            network_matchers,
        });
    }

    fn rename_profile(self: Pin<&mut Self>, id: &QString, name: &QString) {
        let id = id.to_string();
        let Some(existing) = self.store.find_by_id(&id) else {
            tracing::warn!(%id, "ProfilesModel: renameProfile for unknown profile, ignored");
            return;
        };
        let network_matchers = existing.network_matchers.clone();
        self.emit_client(ClientMessage::UpdateProfile {
            id,
            name: name.to_string(),
            network_matchers,
        });
    }

    fn update_matchers(self: Pin<&mut Self>, id: &QString, matchers_csv: &QString) {
        let id = id.to_string();
        let Some(existing) = self.store.find_by_id(&id) else {
            tracing::warn!(%id, "ProfilesModel: updateMatchers for unknown profile, ignored");
            return;
        };
        let name = existing.name.clone();
        let network_matchers = parse_matchers(&matchers_csv.to_string());
        self.emit_client(ClientMessage::UpdateProfile {
            id,
            name,
            network_matchers,
        });
    }

    fn delete_profile(self: Pin<&mut Self>, id: &QString) {
        self.emit_client(ClientMessage::DeleteProfile { id: id.to_string() });
    }

    fn activate_profile(self: Pin<&mut Self>, id: &QString) {
        self.emit_client(ClientMessage::ActivateProfile { id: id.to_string() });
    }

    fn deactivate_profile(self: Pin<&mut Self>) {
        self.emit_client(ClientMessage::DeactivateProfile);
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("ProfilesModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "ProfilesModel",
            crate::bridge_dispatch::interests_profiles,
            move |_msg, json| {
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );
    }
}

impl qobject::ProfilesModel {
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
            Ok(json) => self.as_mut().profile_change_requested(QString::from(&json)),
            Err(e) => {
                tracing::error!(error = %e, "ProfilesModel: client message serialize failed")
            }
        }
    }
}
