//! `GeoModel` — a `QAbstractListModel` exposing the per-country geographic
//! breakdown to QML (`GeoPage.qml`).
//!
//! The pure aggregation logic lives in [`crate::geo::store::GeoStore`] and is
//! unit-tested without Qt, mirroring `ConnectionsModel`/`RowStore`. Unlike
//! `ConnectionsModel`'s incremental `beginInsertRows`/`endInsertRows`
//! replay, this wrapper resets the whole model on every applied message: the
//! aggregate can re-sort (a country moving up/down the list as its count
//! changes) on almost every event, and the list is small (at most one row
//! per ISO country plus the two special buckets), so a full reset is both
//! simpler and cheap — no surgical `ModelOp` replay is needed here the way
//! `RowStore`'s ordered, individually-addressable rows require it.
//!
//! `dbAvailable`/`dbPath` are read once at construction from
//! [`crate::geo::resolver::discover_and_open`] (mirrors `AppInfo`'s
//! construction-time `env!(...)` read) and drive `GeoPage.qml`'s setup
//! placeholder when no GeoLite2-Country database is installed.
//!
//! Live wiring (`start_bridge_feed`) follows `ConnectionsModel`/`TrafficModel`'s
//! shape exactly: subscribe to the bridge's `broadcast::Receiver<ServerMessage>`
//! on its own Tokio task, filter with `bridge_dispatch::interests_geo`, and
//! `qt_thread.queue(...)` onto `applyServerMessageJson`. The one addition
//! specific to this model: before queueing, the feed task also resolves every
//! row's destination IP through the same [`crate::geo::resolver::SharedResolver`]
//! `GeoStore` uses — that's the actual GeoIP database read, done off the Qt
//! thread. By the time the message reaches `GeoStore::apply` on the Qt thread,
//! the resolver's shared cache is already warm, so `apply` never blocks on I/O.

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

use crate::geo::resolver::SharedResolver;
use crate::geo::store::GeoStore;
use snitchwatch_bridge::ws_messages::ServerMessage;

// Role ids exposed to the QML delegate.
const ROLE_COUNTRY_CODE: i32 = 0;
const ROLE_COUNTRY_NAME: i32 = 1;
const ROLE_FLAG: i32 = 2;
const ROLE_TOTAL: i32 = 3;
const ROLE_ALLOWED: i32 = 4;
const ROLE_DENIED: i32 = 5;
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

        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;
    }

    extern "RustQt" {
        /// Per-country connection aggregate, bound by `GeoPage.qml`'s
        /// `ListView`.
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(i32, count)]
        #[qproperty(i32, max_total, cxx_name = "maxTotal")]
        #[qproperty(bool, db_available, cxx_name = "dbAvailable")]
        #[qproperty(QString, db_path, cxx_name = "dbPath")]
        type GeoModel = super::GeoModelRust;

        // --- QAbstractListModel overrides -----------------------------------
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &GeoModel, _parent: &QModelIndex) -> i32;

        #[qinvokable]
        #[cxx_override]
        unsafe fn data(self: &GeoModel, index: &QModelIndex, role: i32) -> QVariant;

        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &GeoModel) -> QHash_i32_QByteArray;

        // --- Feed ingestion (QML/string convenience) ------------------------
        /// Apply one JSON-encoded bridge `ServerMessage`. Convenience wrapper
        /// over the typed `apply_server_message`, callable from QML;
        /// malformed input is logged and ignored.
        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut GeoModel>, json: &QString);

        /// Start the live outbound feed: subscribe to the bridge's
        /// `ServerMessage` broadcast and queue geo-relevant messages onto the
        /// Qt thread via `applyServerMessageJson`, after resolving each row's
        /// destination IP off the Qt thread. No-op when the bridge isn't
        /// running (e.g. headless tests). Called from QML
        /// `Component.onCompleted`; safe to call at most once per instance.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut GeoModel>);
    }

    // Protected base-class helpers inherited from QAbstractListModel.
    unsafe extern "RustQt" {
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut GeoModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut GeoModel>);
    }

    impl cxx_qt::Threading for GeoModel {}
}

/// Rust-side state for [`qobject::GeoModel`].
pub struct GeoModelRust {
    store: GeoStore,
    /// Clone of the same `SharedResolver` `store` holds, kept here so
    /// `start_bridge_feed`'s Tokio task can resolve rows off the Qt thread
    /// while sharing the store's cache (see module docs).
    resolver: SharedResolver,
    /// Materialised, sorted snapshot of `store.display_rows()`; refreshed on
    /// every applied message so `data()`/`row_count()` stay O(1) reads.
    rows: Vec<crate::geo::store::DisplayRow>,
    count: i32,
    max_total: i32,
    db_available: bool,
    db_path: QString,
}

impl Default for GeoModelRust {
    fn default() -> Self {
        let outcome = crate::geo::resolver::discover_and_open();
        let resolver = SharedResolver::new(outcome.lookup);
        Self {
            store: GeoStore::with_resolver(resolver.clone()),
            resolver,
            rows: Vec::new(),
            count: 0,
            max_total: 0,
            db_available: outcome.available,
            db_path: QString::from(&outcome.path.display().to_string()),
        }
    }
}

impl qobject::GeoModel {
    fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }

    unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(row) = self.rows.get(index.row() as usize) else {
            return QVariant::default();
        };
        match role {
            ROLE_COUNTRY_CODE => QVariant::from(&QString::from(&row.country_code)),
            ROLE_COUNTRY_NAME => QVariant::from(&QString::from(&row.country_name)),
            ROLE_FLAG => QVariant::from(&QString::from(&row.flag)),
            ROLE_TOTAL => QVariant::from(&row.total),
            ROLE_ALLOWED => QVariant::from(&row.allowed),
            ROLE_DENIED => QVariant::from(&row.denied),
            ROLE_PENDING => QVariant::from(&row.pending),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_COUNTRY_CODE, QByteArray::from("countryCode"));
        roles.insert(ROLE_COUNTRY_NAME, QByteArray::from("countryName"));
        roles.insert(ROLE_FLAG, QByteArray::from("flag"));
        roles.insert(ROLE_TOTAL, QByteArray::from("total"));
        roles.insert(ROLE_ALLOWED, QByteArray::from("allowed"));
        roles.insert(ROLE_DENIED, QByteArray::from("denied"));
        roles.insert(ROLE_PENDING, QByteArray::from("pending"));
        roles
    }

    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(&msg),
            Err(e) => tracing::warn!(error = %e, "GeoModel: bad ServerMessage JSON"),
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("GeoModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let resolver = self.resolver.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "GeoModel",
            crate::bridge_dispatch::interests_geo,
            move |msg, json| {
                // Resolve every row's destination IP here, off the Qt
                // thread — this is the actual GeoIP database read.
                // `resolver` shares its cache with the Qt-thread-side
                // `GeoStore`, so the queued apply below only ever hits
                // the now-warm cache.
                warm_resolver_cache(&resolver, msg);
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );
    }
}

/// Resolve every connection row's destination IP carried by `msg`, warming
/// `resolver`'s shared cache ahead of the Qt-thread-side `GeoStore::apply`
/// call. A no-op for message variants that carry no rows (removal only
/// touches ids already resolved on a prior insert/update; move/clear carry
/// no IPs at all).
fn warm_resolver_cache(resolver: &SharedResolver, msg: &ServerMessage) {
    match msg {
        ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows } => {
            for row in rows {
                resolver.resolve(&row.dst_ip);
            }
        }
        _ => {}
    }
}

impl qobject::GeoModel {
    /// Apply a typed bridge message. Every applied change resets the whole
    /// model (see module docs for why a full reset, not a surgical replay,
    /// is the right trade-off for this aggregate).
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: &ServerMessage) {
        let changed = self.as_mut().rust_mut().store.apply(msg);
        if !changed {
            return;
        }
        unsafe {
            self.as_mut().begin_reset_model();
        }
        let rows = self.store.display_rows();
        self.as_mut().rust_mut().rows = rows;
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.refresh_counts();
    }

    fn refresh_counts(mut self: Pin<&mut Self>) {
        let count = self.rows.len() as i32;
        let max_total = self.rows.iter().map(|r| r.total).max().unwrap_or(0);
        self.as_mut().set_count(count);
        self.as_mut().set_max_total(max_total);
    }
}
