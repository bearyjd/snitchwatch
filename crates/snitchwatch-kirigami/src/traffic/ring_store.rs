//! Qt-free ring buffer + unit-scaling store for the Traffic tab (Task 11).
//!
//! Wraps the bridge's existing [`TrafficBinner`] (already a fixed-window,
//! Qt-free 1-second-bucket ring buffer, unit-tested in
//! `snitchwatch-bridge/src/cache/traffic_bins.rs`) instead of re-implementing
//! binning here — the bridge only exposes it via the WS `TrafficEvents`
//! message today, so this module's job is: fold that message into the
//! binner, and derive the display-ready bits (current in/out rate, a
//! human-readable label per Task 11's "unit scaling helpers" requirement)
//! that `TrafficModel` (the cxx-qt wrapper) exposes to QML.
//!
//! [`ServerMessage::SetTrafficData`] / [`ServerMessage::UpdateTrafficData`]
//! are deliberately NOT consumed here: those carry an opaque
//! `serde_json::Value` blob shaped for `uPlot`'s columnar format
//! (`web/js/traffic.js`), not a typed, Rust-friendly shape. `TrafficEvents`
//! is the one typed, per-second variant that maps directly onto
//! `TrafficBinner::record`, so it is the sole input this store understands.

use snitchwatch_bridge::cache::traffic_bins::TrafficBinner;
use snitchwatch_bridge::ws_messages::ServerMessage;

/// Default visible window: last 300 seconds (5 minutes) of traffic.
pub const DEFAULT_WINDOW_SECONDS: usize = 300;

/// Fixed-window per-second in/out byte-rate store backing `TrafficModel`.
#[derive(Debug)]
pub struct TrafficStore {
    binner: TrafficBinner,
}

impl Default for TrafficStore {
    fn default() -> Self {
        Self::new(DEFAULT_WINDOW_SECONDS)
    }
}

impl TrafficStore {
    pub fn new(window_seconds: usize) -> Self {
        Self {
            binner: TrafficBinner::new(window_seconds),
        }
    }

    /// Apply one bridge message. Returns `true` if the series changed (the
    /// model wrapper re-serializes/repaints on `true`).
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::TrafficEvents { events } => {
                for e in events {
                    self.binner.record(e.timestamp_ms, e.bytes_in, e.bytes_out);
                }
                !events.is_empty()
            }
            _ => false,
        }
    }

    pub fn len(&self) -> usize {
        self.binner.series().0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `(timestamps_ms, bytes_in_per_sec, bytes_out_per_sec)`, oldest first.
    pub fn series(&self) -> (Vec<i64>, Vec<u64>, Vec<u64>) {
        self.binner.series()
    }

    /// Most recent bucket's in/out byte rate (bytes/sec, since buckets are
    /// fixed 1-second wide); `(0, 0)` when empty.
    pub fn current_rates(&self) -> (u64, u64) {
        let (_, bin, bout) = self.series();
        (
            bin.last().copied().unwrap_or(0),
            bout.last().copied().unwrap_or(0),
        )
    }
}

/// Format a byte rate as a human-readable `"12.3 MB/s"`-style label, mirroring
/// `web/js/traffic.js`'s `fmtRateAxis` scaling thresholds.
pub fn format_rate(bytes_per_sec: u64) -> String {
    format_scaled(bytes_per_sec, "/s")
}

/// Format a raw byte count as `"12.3 MB"`, mirroring `fmtBytesAxis`.
pub fn format_bytes(bytes: u64) -> String {
    format_scaled(bytes, "")
}

fn format_scaled(value: u64, suffix: &str) -> String {
    const KB: f64 = 1e3;
    const MB: f64 = 1e6;
    const GB: f64 = 1e9;
    const TB: f64 = 1e12;

    let v = value as f64;
    if value == 0 {
        return format!("0{suffix}");
    }
    if v < KB {
        format!("{value}B{suffix}")
    } else if v < MB {
        format!("{:.1}kB{suffix}", v / KB)
    } else if v < GB {
        format!("{:.1}MB{suffix}", v / MB)
    } else if v < TB {
        format!("{:.1}GB{suffix}", v / GB)
    } else {
        format!("{:.1}TB{suffix}", v / TB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::TrafficEvent;

    fn event(ts_ms: i64, bin: u64, bout: u64) -> TrafficEvent {
        TrafficEvent {
            timestamp_ms: ts_ms,
            bytes_in: bin,
            bytes_out: bout,
        }
    }

    #[test]
    fn empty_store_has_no_series_and_zero_rates() {
        let s = TrafficStore::new(60);
        assert!(s.is_empty());
        assert_eq!(s.current_rates(), (0, 0));
    }

    #[test]
    fn traffic_events_populate_the_series() {
        let mut s = TrafficStore::new(60);
        assert!(s.apply(&ServerMessage::TrafficEvents {
            events: vec![
                event(1_000_000_000_000, 100, 50),
                event(1_000_000_001_000, 200, 25),
            ],
        }));
        let (ts, bin, bout) = s.series();
        assert_eq!(ts.len(), 2);
        assert_eq!(bin, vec![100, 200]);
        assert_eq!(bout, vec![50, 25]);
        assert_eq!(s.current_rates(), (200, 25));
    }

    #[test]
    fn empty_events_batch_reports_no_change() {
        let mut s = TrafficStore::new(60);
        assert!(!s.apply(&ServerMessage::TrafficEvents { events: vec![] }));
    }

    #[test]
    fn unrelated_message_does_not_change_the_store() {
        let mut s = TrafficStore::new(60);
        assert!(!s.apply(&ServerMessage::ClearConnectionRows));
        assert!(s.is_empty());
    }

    #[test]
    fn window_evicts_oldest_bucket() {
        let mut s = TrafficStore::new(3);
        let events = (0..5)
            .map(|i| event(1_000_000_000_000 + i * 1000, i as u64, 0))
            .collect();
        s.apply(&ServerMessage::TrafficEvents { events });
        assert_eq!(s.len(), 3);
        let (_, bin, _) = s.series();
        assert_eq!(bin, vec![2, 3, 4]);
    }

    #[test]
    fn format_rate_scales_units() {
        assert_eq!(format_rate(0), "0/s");
        assert_eq!(format_rate(42), "42B/s");
        assert_eq!(format_rate(4_200), "4.2kB/s");
        assert_eq!(format_rate(4_200_000), "4.2MB/s");
        assert_eq!(format_rate(4_200_000_000), "4.2GB/s");
        assert_eq!(format_rate(4_200_000_000_000), "4.2TB/s");
    }

    #[test]
    fn format_bytes_scales_units_without_per_second_suffix() {
        assert_eq!(format_bytes(0), "0");
        assert_eq!(format_bytes(1_500), "1.5kB");
    }

    #[test]
    fn bridge_serialized_traffic_events_round_trip_into_the_store() {
        // Cross-crate end-to-end shape check (bridge TrafficEvents wiring):
        // build the exact events the bridge's outbound traffic pump would
        // compute from a connection row's byte counters
        // (`snitchwatch_bridge::cache::traffic_tracker::TrafficTracker`,
        // wrapping the same `TrafficBinner` this store wraps), serialize them
        // the same way the bridge's outbound feed does (plain
        // `serde_json::to_string`, mirroring
        // `snitchwatch_kirigami::bridge_dispatch::encode_server`), and
        // confirm the resulting JSON deserializes and folds into this store
        // exactly like `TrafficModel::applyServerMessageJson` does in
        // production.
        use snitchwatch_bridge::cache::traffic_tracker::TrafficTracker;
        use snitchwatch_bridge::ws_messages::ConnectionRow;

        let row = ConnectionRow {
            id: "ask-1".into(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent: 1234,
            bytes_received: 5678,
            started_at_ms: 0,
            matched_rule: None,
        };
        let mut tracker = TrafficTracker::new(60);
        let events = tracker.record_rows(1_000_000_000_000, &[row]);
        let bridge_msg = ServerMessage::TrafficEvents { events };

        let json = serde_json::to_string(&bridge_msg).expect("bridge-side encode");
        let decoded: ServerMessage = serde_json::from_str(&json).expect("kirigami-side decode");

        let mut store = TrafficStore::new(60);
        assert!(store.apply(&decoded));
        assert_eq!(store.current_rates(), (5678, 1234));
    }
}
