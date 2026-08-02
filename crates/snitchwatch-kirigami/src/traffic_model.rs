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
        // Daemon aggregate stats (issue #19) — the Traffic tab's primary
        // content now, since `Connection` carries no byte counters. `false`
        // until the first `DaemonStatistics` message arrives, driving the
        // QML placeholder.
        #[qproperty(bool, stats_received, cxx_name = "statsReceived")]
        #[qproperty(QString, daemon_version, cxx_name = "daemonVersion")]
        #[qproperty(i32, uptime_secs, cxx_name = "uptimeSecs")]
        #[qproperty(i32, stat_connections, cxx_name = "statConnections")]
        #[qproperty(i32, stat_accepted, cxx_name = "statAccepted")]
        #[qproperty(i32, stat_dropped, cxx_name = "statDropped")]
        #[qproperty(i32, stat_rule_hits, cxx_name = "statRuleHits")]
        #[qproperty(i32, stat_rule_misses, cxx_name = "statRuleMisses")]
        #[qproperty(i32, stat_rules, cxx_name = "statRules")]
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
    stats_received: bool,
    daemon_version: QString,
    uptime_secs: i32,
    stat_connections: i32,
    stat_accepted: i32,
    stat_dropped: i32,
    stat_rule_hits: i32,
    stat_rule_misses: i32,
    stat_rules: i32,
}

impl Default for TrafficModelRust {
    fn default() -> Self {
        Self {
            store: TrafficStore::default(),
            count: 0,
            series_json: QString::from("{\"timestampsMs\":[],\"bytesIn\":[],\"bytesOut\":[]}"),
            current_in_label: QString::from("--"),
            current_out_label: QString::from("--"),
            stats_received: false,
            daemon_version: QString::from(""),
            uptime_secs: 0,
            stat_connections: 0,
            stat_accepted: 0,
            stat_dropped: 0,
            stat_rule_hits: 0,
            stat_rule_misses: 0,
            stat_rules: 0,
        }
    }
}

/// Daemon `Statistics` counters are `u64`; QML's own numeric qproperty
/// convention across this crate's models is `i32` (see e.g.
/// `ConnectionsModel::count`/`totalCount`) — clamp rather than wrap so a
/// (practically unreachable) huge counter degrades to a display cap instead
/// of a garbage negative number.
fn u64_to_display_i32(value: u64) -> i32 {
    value.min(i32::MAX as u64) as i32
}

/// The subset of `ServerMessage::DaemonStatistics` the Traffic tab displays,
/// pre-converted to the qproperty-facing `i32` widths. Pulled out as a pure
/// function so the field mapping is unit-testable without a Qt runtime — the
/// cxx-qt qobject methods below only need to apply it to properties.
#[derive(Debug, Clone, PartialEq)]
struct StatsDisplay {
    daemon_version: String,
    uptime_secs: i32,
    stat_connections: i32,
    stat_accepted: i32,
    stat_dropped: i32,
    stat_rule_hits: i32,
    stat_rule_misses: i32,
    stat_rules: i32,
}

fn stats_display_from_message(msg: &ServerMessage) -> Option<StatsDisplay> {
    match msg {
        ServerMessage::DaemonStatistics {
            daemon_version,
            uptime,
            rules,
            connections,
            ignored: _,
            accepted,
            dropped,
            rule_hits,
            rule_misses,
        } => Some(StatsDisplay {
            daemon_version: daemon_version.clone(),
            uptime_secs: u64_to_display_i32(*uptime),
            stat_connections: u64_to_display_i32(*connections),
            stat_accepted: u64_to_display_i32(*accepted),
            stat_dropped: u64_to_display_i32(*dropped),
            stat_rule_hits: u64_to_display_i32(*rule_hits),
            stat_rule_misses: u64_to_display_i32(*rule_misses),
            stat_rules: u64_to_display_i32(*rules),
        }),
        _ => None,
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
        if let Some(stats) = stats_display_from_message(&msg) {
            self.as_mut()
                .set_daemon_version(QString::from(stats.daemon_version.as_str()));
            self.as_mut().set_uptime_secs(stats.uptime_secs);
            self.as_mut().set_stat_connections(stats.stat_connections);
            self.as_mut().set_stat_accepted(stats.stat_accepted);
            self.as_mut().set_stat_dropped(stats.stat_dropped);
            self.as_mut().set_stat_rule_hits(stats.stat_rule_hits);
            self.as_mut().set_stat_rule_misses(stats.stat_rule_misses);
            self.as_mut().set_stat_rules(stats.stat_rules);
            self.as_mut().set_stats_received(true);
            return;
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_display_from_message_maps_daemon_statistics_fields() {
        let json = r#"{
            "action": "daemonStatistics",
            "daemonVersion": "1.8.0",
            "uptime": 3661,
            "rules": 12,
            "connections": 4200,
            "ignored": 10,
            "accepted": 4000,
            "dropped": 200,
            "ruleHits": 3900,
            "ruleMisses": 300
        }"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        let stats = stats_display_from_message(&msg).expect("must recognize DaemonStatistics");
        assert_eq!(
            stats,
            StatsDisplay {
                daemon_version: "1.8.0".to_string(),
                uptime_secs: 3661,
                stat_connections: 4200,
                stat_accepted: 4000,
                stat_dropped: 200,
                stat_rule_hits: 3900,
                stat_rule_misses: 300,
                stat_rules: 12,
            }
        );
    }

    #[test]
    fn stats_display_from_message_ignores_other_variants() {
        let msg = ServerMessage::TrafficEvents {
            events: vec![snitchwatch_bridge::ws_messages::TrafficEvent {
                timestamp_ms: 1_000_000_000_000,
                bytes_in: 100,
                bytes_out: 50,
            }],
        };
        assert_eq!(stats_display_from_message(&msg), None);
    }

    #[test]
    fn u64_to_display_i32_clamps_at_i32_max() {
        assert_eq!(u64_to_display_i32(42), 42);
        assert_eq!(u64_to_display_i32(u64::MAX), i32::MAX);
    }
}
