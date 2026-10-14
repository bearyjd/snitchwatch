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

use crate::curated_broad_consent::{BroadConsent, DISCLOSURE, DNS_ID};
use crate::curated_defaults::{entry_status_text, held_back, CuratedStore};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

const ROLE_ID: i32 = 0;
const ROLE_PROGRAM: i32 = 1;
const ROLE_ALLOWS: i32 = 2;
const ROLE_WHY: i32 = 3;
const ROLE_ON: i32 = 4;
const ROLE_STATUS: i32 = 5;
const ROLE_PROBLEM: i32 = 6;
const ROLE_CAN_REMOVE: i32 = 7;
const ROLE_CAN_KEEP: i32 = 8;
const ROLE_BROAD: i32 = 9;
const ROLE_GROUP_TITLE: i32 = 10;
const ROLE_HELD_BACK: i32 = 11;

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
        #[qproperty(bool, broad_supported, cxx_name = "broadSupported")]
        #[qproperty(bool, consent_open, cxx_name = "consentOpen")]
        #[qproperty(bool, consent_ready, cxx_name = "consentReady")]
        #[qproperty(QString, consent_disclosure, cxx_name = "consentDisclosure")]
        #[qproperty(QString, consent_result, cxx_name = "consentResult")]
        #[qproperty(i32, count)]
        #[qproperty(bool, received)]
        #[qproperty(bool, grouped)]
        #[qproperty(QString, groups_json, cxx_name = "groupsJson")]
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

        #[qinvokable]
        #[cxx_name = "confirmBroadConsent"]
        fn confirm_broad_consent(self: Pin<&mut CuratedDefaultsModel>);
        #[qinvokable]
        #[cxx_name = "cancelBroadConsent"]
        fn cancel_broad_consent(self: Pin<&mut CuratedDefaultsModel>);
        /// Clear local DNS choice correlation when another page requests Off.
        #[qinvokable]
        #[cxx_name = "revokeBroadConsent"]
        fn revoke_broad_consent(self: Pin<&mut CuratedDefaultsModel>);
        #[qinvokable]
        #[cxx_name = "reenableDns"]
        fn reenable_dns(self: Pin<&mut CuratedDefaultsModel>);
        #[qinvokable]
        #[cxx_name = "resetBridgeSession"]
        fn reset_bridge_session(self: Pin<&mut CuratedDefaultsModel>);

        /// Ask to turn one entry on or off.
        #[qinvokable]
        #[cxx_name = "setEntry"]
        fn set_entry(self: Pin<&mut CuratedDefaultsModel>, id: &QString, on: bool);

        /// Ask to turn one group on or off (`groupsJson`'s `id`): one request
        /// with the ids of its entries not already that way. The DNS group
        /// goes through the reviewed consent flow instead.
        #[qinvokable]
        #[cxx_name = "setGroup"]
        fn set_group(self: Pin<&mut CuratedDefaultsModel>, id: &QString, on: bool);

        /// Demo mode only (`SNITCHWATCH_DEMO=1`): load fake grouped rules.
        #[qinvokable]
        #[cxx_name = "loadDemoGroups"]
        fn load_demo_groups(self: Pin<&mut CuratedDefaultsModel>);

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

        /// Keep an undecided rule already in the firewall (turn it on, so
        /// it is adopted; nothing is sent). Ignored for any other entry.
        #[qinvokable]
        #[cxx_name = "keepEntry"]
        fn keep_entry(self: Pin<&mut CuratedDefaultsModel>, id: &QString);
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
    consent: BroadConsent,
    broad_supported: bool,
    consent_open: bool,
    consent_ready: bool,
    consent_disclosure: QString,
    consent_result: QString,
    count: i32,
    received: bool,
    grouped: bool,
    groups_json: QString,
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
            ROLE_STATUS => text(entry_status_text(entry)),
            ROLE_PROBLEM => text(entry.problem.as_deref().unwrap_or_default()),
            ROLE_CAN_REMOVE => QVariant::from(&self.store.can_remove(entry)),
            ROLE_BROAD => QVariant::from(&(entry.broad || entry.id == DNS_ID)),
            ROLE_CAN_KEEP => QVariant::from(&self.store.can_keep(entry)),
            ROLE_GROUP_TITLE => text(self.store.group_title(entry)),
            ROLE_HELD_BACK => QVariant::from(&held_back(entry)),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_BROAD, QByteArray::from("isBroad"));
        roles.insert(ROLE_ID, QByteArray::from("entryId"));
        roles.insert(ROLE_PROGRAM, QByteArray::from("program"));
        roles.insert(ROLE_ALLOWS, QByteArray::from("allows"));
        roles.insert(ROLE_WHY, QByteArray::from("why"));
        // Not `checked`/`on`: avoid names a delegate's controls declare.
        roles.insert(ROLE_ON, QByteArray::from("isOn"));
        roles.insert(ROLE_STATUS, QByteArray::from("statusText"));
        roles.insert(ROLE_PROBLEM, QByteArray::from("problem"));
        roles.insert(ROLE_CAN_REMOVE, QByteArray::from("canRemove"));
        roles.insert(ROLE_CAN_KEEP, QByteArray::from("canKeep"));
        roles.insert(ROLE_GROUP_TITLE, QByteArray::from("groupTitle"));
        roles.insert(ROLE_HELD_BACK, QByteArray::from("heldBack"));
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
        if let ServerMessage::Authenticated { capabilities } = &msg {
            self.as_mut().rust_mut().consent.authenticate(
                connection_id,
                capabilities
                    .iter()
                    .any(|c| c == crate::curated_broad_consent::CAPABILITY),
            );
        }
        if let Ok(value) = serde_json::to_value(&msg) {
            match value["action"].as_str() {
                Some("curatedBroadOptInPrepared") => {
                    self.as_mut()
                        .rust_mut()
                        .consent
                        .prepared(connection_id, &value);
                }
                Some("curatedDefaultsRequestResult") => {
                    self.as_mut()
                        .rust_mut()
                        .consent
                        .receive_result(connection_id, &value);
                }
                _ => {}
            }
        }
        self.as_mut().sync_consent();
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
        self.as_mut().set_grouped(next.grouped());
        self.as_mut()
            .set_groups_json(QString::from(&next.groups_json()));
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
                    let capable = session_handles.advertises_curated_broad_opt_in(connection_id);
                    let mut qobject = qobject;
                    qobject
                        .as_mut()
                        .rust_mut()
                        .consent
                        .authenticate(connection_id, capable);
                    qobject.apply_session_message(connection_id, &QString::from(&json));
                });
            },
        );
    }

    fn set_entry(mut self: Pin<&mut Self>, id: &QString, on: bool) {
        if id.to_string() == DNS_ID && on {
            self.begin_broad("enable");
            return;
        }
        if id.to_string() == DNS_ID && !on {
            self.as_mut().cancel_broad_consent();
            self.as_mut().rust_mut().consent.revoke_local();
            self.as_mut().sync_consent();
        }
        let request = self.store.request(&id.to_string(), on);
        self.emit_client(request);
    }

    fn set_group(mut self: Pin<&mut Self>, id: &QString, on: bool) {
        let gid = id.to_string();
        if self.store.group_is_broad(&gid) {
            for entry in self.store.group_broad_ids(&gid) {
                self.as_mut().set_entry(&QString::from(&entry), on);
            }
            return;
        }
        let request = self.store.request_group(&gid, on);
        self.emit_client(request);
    }

    fn load_demo_groups(mut self: Pin<&mut Self>) {
        if !crate::demo::enabled() {
            return;
        }
        let session = self.store.session();
        for json in crate::demo::curated_group_messages() {
            self.as_mut()
                .apply_session_message(session, &QString::from(&json));
        }
    }

    fn set_all(mut self: Pin<&mut Self>, on: bool) {
        if !on {
            self.as_mut().cancel_broad_consent();
            self.as_mut().rust_mut().consent.revoke_local();
            self.as_mut().sync_consent();
        }
        let request = self.store.request_all(on);
        self.emit_client(request);
    }

    fn remove_entry(mut self: Pin<&mut Self>, id: &QString) {
        if id.to_string() == DNS_ID {
            self.as_mut().cancel_broad_consent();
            self.as_mut().rust_mut().consent.revoke_local();
            self.as_mut().sync_consent();
        }
        let request = self.store.removal(&id.to_string());
        self.emit_client(request);
    }

    fn keep_entry(self: Pin<&mut Self>, id: &QString) {
        if id.to_string() == DNS_ID {
            self.begin_broad("keep");
            return;
        }
        let request = self.store.keeping(&id.to_string());
        self.emit_client(request);
    }

    fn sync_consent(mut self: Pin<&mut Self>) {
        let consent = self.consent.clone();
        self.as_mut().set_broad_supported(consent.supported());
        self.as_mut().set_consent_open(consent.open());
        self.as_mut().set_consent_ready(consent.ready());
        self.as_mut()
            .set_consent_disclosure(QString::from(DISCLOSURE));
        self.as_mut()
            .set_consent_result(QString::from(consent.result()));
    }

    fn begin_broad(mut self: Pin<&mut Self>, purpose: &str) {
        use snitchwatch_bridge::curated::reconcile::EntryStatus;
        let entry = (0..self.store.len())
            .filter_map(|i| self.store.row(i))
            .find(|e| e.id == DNS_ID);
        let purpose = if purpose == "enable"
            && entry.is_some_and(|e| {
                matches!(
                    e.status,
                    EntryStatus::InstalledButOff | EntryStatus::InFirewallButOff
                )
            }) {
            "reenable"
        } else {
            purpose
        };
        let allowed = entry.is_some_and(|e| {
            self.store.unavailable_reason().is_empty()
                && match purpose {
                    "keep" => self.store.can_keep(e),
                    "reenable" => matches!(
                        e.status,
                        EntryStatus::InstalledButOff | EntryStatus::InFirewallButOff
                    ),
                    "enable" => matches!(
                        e.status,
                        EntryStatus::Off
                            | EntryStatus::NotInstalled
                            | EntryStatus::Installed
                            | EntryStatus::AwaitingConsent
                    ),
                    _ => false,
                }
        });
        if !allowed {
            return;
        }
        self.as_mut().cancel_broad_consent();
        let session = self.store.session();
        if let Some(handles) = crate::bridge_runtime::handles() {
            if !handles.advertises_curated_broad_opt_in(session) {
                self.as_mut().reset_bridge_session();
                return;
            }
        }
        let request = self.as_mut().rust_mut().consent.begin(purpose);
        if let Some(request) = request {
            self.as_mut().send_broad(session, request);
        }
        self.sync_consent();
    }

    fn send_broad(mut self: Pin<&mut Self>, session: u64, value: serde_json::Value) {
        let result = serde_json::from_value::<ClientMessage>(value);
        let Ok(message) = result else {
            self.as_mut()
                .rust_mut()
                .consent
                .failed("This bridge version cannot accept reviewed DNS consent.");
            return;
        };
        if let Some(handles) = crate::bridge_runtime::handles() {
            if handles.try_send_for_session(session, message).is_err() {
                self.as_mut()
                    .rust_mut()
                    .consent
                    .failed("DNS request was not queued. Open a fresh confirmation to try again.");
            }
        } else {
            // Offline QML probes observe the exact JSON; the production runtime
            // always sends reviewed intent directly with its original session.
            self.emit_client(Some(message));
        }
    }

    fn confirm_broad_consent(mut self: Pin<&mut Self>) {
        let request = self.as_mut().rust_mut().consent.confirm();
        if let Some((session, request)) = request {
            self.as_mut().send_broad(session, request);
        }
        self.sync_consent();
    }

    fn cancel_broad_consent(mut self: Pin<&mut Self>) {
        let request = self.as_mut().rust_mut().consent.cancel();
        if let Some((session, request)) = request {
            self.as_mut().send_broad(session, request);
        }
        self.sync_consent();
    }

    fn revoke_broad_consent(mut self: Pin<&mut Self>) {
        self.as_mut().cancel_broad_consent();
        self.as_mut().rust_mut().consent.revoke_local();
        self.sync_consent();
    }

    fn reenable_dns(self: Pin<&mut Self>) {
        self.begin_broad("reenable");
    }

    fn reset_bridge_session(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().consent.disconnect();
        self.sync_consent();
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
