//! `TrafficModel` — the `QObject` backing the Traffic tab's chart (Task 11).
//!
//! Unlike the other tabs, this is not a `QAbstractListModel`: the chart is a
//! QML `Canvas` (see `TrafficPage.qml` and the module docs on
//! [`crate::traffic::ring_store`] for why — no `QtCharts`/`QtGraphs` QML
//! module is available in this environment) that repaints from a JSON-encoded
//! series property, plus a couple of human-readable current-rate labels for
//! the readout above the chart.
//!
//! The pure fold logic lives in [`crate::traffic::ring_store`] and is
//! unit-tested without Qt. This is the thin cxx-qt wrapper, following the
//! same live-feed pattern as `BlocklistsModel` (Task 13).

use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use serde::Serialize;

use crate::traffic::ring_store::{format_rate, TrafficStore};
use snitchwatch_bridge::ws_messages::ServerMessage;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Traffic chart data + readout, bound by `TrafficPage.qml`.
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        #[qproperty(QString, series_json, cxx_name = "seriesJson")]
        #[qproperty(QString, current_in_label, cxx_name = "currentInLabel")]
        #[qproperty(QString, current_out_label, cxx_name = "currentOutLabel")]
        type TrafficModel = super::TrafficModelRust;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut TrafficModel>, json: &QString);

        /// Start the live outbound feed (Task 13): subscribe to the bridge's
        /// `ServerMessage` broadcast and queue traffic-event messages onto the
        /// Qt thread. No-op when the bridge isn't running. Called from QML
        /// `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut TrafficModel>);
    }

    impl cxx_qt::Threading for TrafficModel {}
}

/// JSON shape handed to QML's `Canvas` paint handler.
#[derive(Serialize)]
struct SeriesPayload<'a> {
    #[serde(rename = "timestampsMs")]
    timestamps_ms: &'a [i64],
    #[serde(rename = "bytesIn")]
    bytes_in: &'a [u64],
    #[serde(rename = "bytesOut")]
    bytes_out: &'a [u64],
}

/// Rust-side state for [`qobject::TrafficModel`].
pub struct TrafficModelRust {
    store: TrafficStore,
    count: i32,
    series_json: QString,
    current_in_label: QString,
    current_out_label: QString,
}

impl Default for TrafficModelRust {
    fn default() -> Self {
        Self {
            store: TrafficStore::default(),
            count: 0,
            series_json: QString::from("{\"timestampsMs\":[],\"bytesIn\":[],\"bytesOut\":[]}"),
            current_in_label: QString::from("--"),
            current_out_label: QString::from("--"),
        }
    }
}

impl qobject::TrafficModel {
    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "TrafficModel: bad ServerMessage JSON"),
        }
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("TrafficModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "TrafficModel",
            crate::bridge_dispatch::interests_traffic,
            move |_msg, json| {
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );
    }
}

impl qobject::TrafficModel {
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: ServerMessage) {
        let changed = self.as_mut().rust_mut().store.apply(&msg);
        if !changed {
            return;
        }
        let (ts, bin, bout) = self.store.series();
        let payload = SeriesPayload {
            timestamps_ms: &ts,
            bytes_in: &bin,
            bytes_out: &bout,
        };
        let json = serde_json::to_string(&payload).unwrap_or_else(|e| {
            tracing::error!(error = %e, "TrafficModel: series serialize failed");
            "{\"timestampsMs\":[],\"bytesIn\":[],\"bytesOut\":[]}".to_string()
        });
        let (in_rate, out_rate) = self.store.current_rates();
        let count = self.store.len() as i32;

        self.as_mut().set_series_json(QString::from(&json));
        self.as_mut()
            .set_current_in_label(QString::from(&format_rate(in_rate)));
        self.as_mut()
            .set_current_out_label(QString::from(&format_rate(out_rate)));
        self.as_mut().set_count(count);
    }
}
