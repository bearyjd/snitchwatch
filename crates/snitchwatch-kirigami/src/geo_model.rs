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
//! Live wiring prepares a bounded typed payload off the Qt thread. It carries
//! authoritative per-ID geographic results and activity codes independently of
//! shared-cache residency. The Qt thread applies those supplied facts without
//! resolving IPs, including snapshots larger than the bounded resolver cache.
//! Direct JSON injection remains an explicit synchronous fixture/demo path.

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QUrl, QVariant,
};

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
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
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
        #[qproperty(bool, capacity_reached, cxx_name = "capacityReached")]
        #[qproperty(i32, max_total, cxx_name = "maxTotal")]
        #[qproperty(bool, db_available, cxx_name = "dbAvailable")]
        #[qproperty(bool, db_city_available, cxx_name = "dbCityAvailable")]
        #[qproperty(QString, db_city_install_path, cxx_name = "dbCityInstallPath")]
        #[qproperty(QString, db_path, cxx_name = "dbPath")]
        #[qproperty(QString, db_source, cxx_name = "dbSource")]
        #[qproperty(f64, db_build_epoch, cxx_name = "dbBuildEpoch")]
        #[qproperty(QString, db_install_path, cxx_name = "dbInstallPath")]
        #[qproperty(bool, db_install_busy, cxx_name = "dbInstallBusy")]
        #[qproperty(bool, db_restart_required, cxx_name = "dbRestartRequired")]
        #[qproperty(QString, db_install_error, cxx_name = "dbInstallError")]
        type GeoModel = super::GeoModelRust;

        #[qinvokable]
        #[cxx_name = "installDatabase"]
        fn install_database(self: Pin<&mut GeoModel>, url: &QUrl);

        #[qinvokable]
        #[cxx_name = "installCityDatabase"]
        fn install_city_database(self: Pin<&mut GeoModel>, url: &QUrl);

        /// Versioned observed country -> city/unavailable -> exact IP hierarchy.
        #[qinvokable]
        #[cxx_name = "hierarchyJson"]
        fn hierarchy_json(self: &GeoModel) -> QString;

        #[qsignal]
        #[cxx_name = "hierarchyChanged"]
        fn hierarchy_changed(self: Pin<&mut GeoModel>);

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

        /// Demo mode only (`SNITCHWATCH_DEMO=1`, ignored otherwise): feed the
        /// fake demo rows through the demo country lookup so the map shows
        /// several countries without a bridge or a GeoIP database.
        #[qinvokable]
        #[cxx_name = "loadDemoRows"]
        fn load_demo_rows(self: Pin<&mut GeoModel>);

        /// Every country outline as one SVG path on the 3600x1800 grid.
        #[qinvokable]
        #[cxx_name = "landPath"]
        fn land_path(self: &GeoModel) -> QString;

        /// JSON array of marker clusters for the map band at `scale` screen
        /// pixels per grid unit, merging markers within `radius_px`, then any closer than `gap_px`.
        #[qinvokable]
        #[cxx_name = "clustersJson"]
        fn clusters_json(self: &GeoModel, scale: f64, radius_px: f64, gap_px: f64) -> QString;

        /// A connection event touched this country (inserted or updated
        /// row). Drives the map's pulse; carries no volume.
        #[qsignal]
        #[cxx_name = "countryActivity"]
        fn country_activity(self: Pin<&mut GeoModel>, code: &QString);
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
    capacity_reached: bool,
    max_total: i32,
    db_available: bool,
    db_city_available: bool,
    db_city_install_path: QString,
    db_path: QString,
    db_source: QString,
    db_build_epoch: f64,
    db_install_path: QString,
    db_install_busy: bool,
    db_restart_required: bool,
    db_install_error: QString,
}

impl Default for GeoModelRust {
    fn default() -> Self {
        let (resolver, outcome) = crate::geo::shared_discovery();
        Self {
            store: GeoStore::with_resolver(resolver.clone()),
            resolver: resolver.clone(),
            rows: Vec::new(),
            count: 0,
            capacity_reached: false,
            max_total: 0,
            db_available: outcome.available,
            db_city_available: resolver.capability() == crate::geo::resolver::GeoCapability::City,
            db_city_install_path: QString::from(
                &crate::geo::install::target_path_for(crate::geo::install::DatabaseProfile::City)
                    .display()
                    .to_string(),
            ),
            db_path: QString::from(&outcome.path.display().to_string()),
            db_source: QString::from(&outcome.source),
            db_build_epoch: outcome.build_epoch as f64,
            db_install_path: QString::from(
                &crate::geo::install::target_path().display().to_string(),
            ),
            db_install_busy: false,
            db_restart_required: false,
            db_install_error: QString::default(),
        }
    }
}

impl qobject::GeoModel {
    fn hierarchy_json(&self) -> QString {
        QString::from(&self.store.hierarchy_json())
    }

    fn install_database(self: Pin<&mut Self>, url: &QUrl) {
        self.start_install(url, crate::geo::install::DatabaseProfile::Country);
    }

    fn install_city_database(self: Pin<&mut Self>, url: &QUrl) {
        self.start_install(url, crate::geo::install::DatabaseProfile::City);
    }

    fn start_install(
        mut self: Pin<&mut Self>,
        url: &QUrl,
        profile: crate::geo::install::DatabaseProfile,
    ) {
        if self.db_install_busy {
            return;
        }
        let Some(source) = url.to_local_file() else {
            self.set_db_install_error(QString::from("Choose a local .mmdb file."));
            return;
        };
        let source = std::path::PathBuf::from(source.to_string());
        self.as_mut().set_db_install_busy(true);
        self.as_mut().set_db_install_error(QString::default());
        let qt_thread = self.qt_thread();
        let spawned = std::thread::Builder::new()
            .name("geo-database-install".into())
            .spawn(move || {
                let result = crate::geo::install::install_selected(&source, profile);
                let _ = qt_thread.queue(move |mut model| {
                    model.as_mut().set_db_install_busy(false);
                    match result {
                        Ok(()) => model.as_mut().set_db_restart_required(true),
                        Err(error) => model.as_mut().set_db_install_error(QString::from(&error)),
                    }
                });
            });
        if let Err(error) = spawned {
            self.as_mut().set_db_install_busy(false);
            self.as_mut().set_db_install_error(QString::from(&format!(
                "Could not start database installation: {error}"
            )));
        }
    }

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

    fn load_demo_rows(self: Pin<&mut Self>) {
        if !crate::demo::enabled() {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        self.apply_server_message(&ServerMessage::InsertConnectionRows {
            rows: crate::demo::demo_rows(now_ms),
        });
    }

    fn land_path(&self) -> QString {
        QString::from(crate::geo::map_markers::land_path())
    }

    fn clusters_json(&self, scale: f64, radius_px: f64, gap_px: f64) -> QString {
        QString::from(&crate::geo::map_markers::clusters_json(
            &self.rows, scale, radius_px, gap_px,
        ))
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("GeoModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let preparer = std::sync::Mutex::new(crate::geo::prepared::GeoMessagePreparer::new(
            self.resolver.clone(),
        ));
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "GeoModel",
            crate::bridge_dispatch::interests_geo,
            move |connection_id, msg, _json| {
                if !session_handles.is_current_session(connection_id) {
                    return;
                }
                let mut prepared = preparer.lock().unwrap().prepare(connection_id, msg);
                prepared.qualify_ids(connection_id);
                let session_handles = session_handles.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if !session_handles.is_current_session(connection_id) {
                        return;
                    }
                    qobject.apply_prepared_message(prepared);
                });
            },
        );
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
        let activity = crate::geo::map_markers::activity_codes(msg, &self.resolver);
        unsafe {
            self.as_mut().begin_reset_model();
        }
        let rows = self.store.display_rows();
        self.as_mut().rust_mut().rows = rows;
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.as_mut().refresh_counts();
        self.as_mut().hierarchy_changed();
        for code in activity {
            self.as_mut().country_activity(&QString::from(&code));
        }
    }

    fn apply_prepared_message(
        mut self: Pin<&mut Self>,
        prepared: crate::geo::prepared::PreparedGeoMessage,
    ) {
        if !self.as_mut().rust_mut().store.apply_prepared(&prepared) {
            return;
        }
        unsafe {
            self.as_mut().begin_reset_model();
        }
        self.as_mut().rust_mut().rows = self.store.display_rows();
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.as_mut().refresh_counts();
        self.as_mut().hierarchy_changed();
        for code in prepared.activity_codes {
            self.as_mut().country_activity(&QString::from(&code));
        }
    }

    fn refresh_counts(mut self: Pin<&mut Self>) {
        let count = self.rows.len() as i32;
        let max_total = self.rows.iter().map(|r| r.total).max().unwrap_or(0);
        let capacity_reached = self.store.capacity_reached();
        self.as_mut().set_capacity_reached(capacity_reached);
        self.as_mut().set_count(count);
        self.as_mut().set_max_total(max_total);
    }
}
