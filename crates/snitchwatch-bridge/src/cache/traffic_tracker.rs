//! Bridge-side glue: fold connection-row byte counters into a shared
//! [`TrafficBinner`] and derive [`TrafficEvent`]s for broadcast as
//! [`ServerMessage::TrafficEvents`] — the one typed, per-second traffic
//! variant (see `snitchwatch-kirigami::traffic::ring_store`'s module docs for
//! the consumer side of this same [`TrafficBinner`], which today only reads
//! it via that message).
//!
//! Additive only: this module doesn't touch
//! [`ServerMessage::SetTrafficData`]/[`ServerMessage::UpdateTrafficData`] (the
//! legacy `uPlot`-shaped blobs the vendored web frontend expects) — nothing
//! here changes whether or when those are sent.

use super::traffic_bins::TrafficBinner;
use crate::ws_messages::{ConnectionRow, TrafficEvent};

/// Wraps [`TrafficBinner`] and derives a [`TrafficEvent`] for the bucket a
/// sample landed in, so callers can broadcast just the (possibly-updated)
/// current bucket rather than replaying the whole window on every sample.
pub struct TrafficTracker {
    binner: TrafficBinner,
}

impl TrafficTracker {
    pub fn new(window_seconds: usize) -> Self {
        Self {
            binner: TrafficBinner::new(window_seconds),
        }
    }

    /// Record one sample at `timestamp_ms` and return the event for the
    /// bucket it landed in (the newest bucket after recording).
    pub fn record(&mut self, timestamp_ms: i64, bytes_in: u64, bytes_out: u64) -> TrafficEvent {
        self.binner.record(timestamp_ms, bytes_in, bytes_out);
        let (ts, bin, bout) = self.binner.series();
        let last = ts.len() - 1;
        TrafficEvent {
            timestamp_ms: ts[last],
            bytes_in: bin[last],
            bytes_out: bout[last],
        }
    }

    /// Record every row's byte counters from a connection-row batch
    /// (`InsertConnectionRows`/`UpdateConnectionRows`) at `timestamp_ms`,
    /// returning one [`TrafficEvent`] per row in the same order.
    /// `ConnectionRow::bytes_sent` (the monitored process's outbound bytes)
    /// maps to `TrafficEvent::bytes_out`; `bytes_received` maps to
    /// `bytes_in`, mirroring `ConnectionRow`'s own field naming.
    pub fn record_rows(&mut self, timestamp_ms: i64, rows: &[ConnectionRow]) -> Vec<TrafficEvent> {
        rows.iter()
            .map(|row| self.record(timestamp_ms, row.bytes_received, row.bytes_sent))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(bytes_sent: u64, bytes_received: u64) -> ConnectionRow {
        ConnectionRow {
            id: "r1".into(),
            process: "curl".into(),
            process_path: None,
            dst_host: "example.com".into(),
            dst_ip: "1.2.3.4".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: None,
            bytes_sent,
            bytes_received,
            started_at_ms: 0,
        }
    }

    #[test]
    fn record_maps_sent_to_out_and_received_to_in() {
        let mut tracker = TrafficTracker::new(60);
        let event = tracker.record(1_000_000_000_000, 200, 100);
        assert_eq!(event.timestamp_ms, 1_000_000_000_000);
        assert_eq!(event.bytes_in, 200);
        assert_eq!(event.bytes_out, 100);
    }

    #[test]
    fn record_rows_maps_each_row_and_preserves_order() {
        let mut tracker = TrafficTracker::new(60);
        let rows = vec![row(10, 20), row(5, 5)];
        let events = tracker.record_rows(1_000_000_000_000, &rows);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].bytes_out, 10);
        assert_eq!(events[0].bytes_in, 20);
        // Same-second samples accumulate in the shared binner, so the second
        // event reflects the cumulative bucket total, not just its own row's
        // bytes — this is `TrafficBinner::record`'s existing
        // same-bucket-aggregation behavior, not new logic invented here.
        assert_eq!(events[1].bytes_out, 15);
        assert_eq!(events[1].bytes_in, 25);
    }

    #[test]
    fn accumulates_across_multiple_records_within_same_second() {
        let mut tracker = TrafficTracker::new(60);
        tracker.record(1_000_000_000_000, 100, 50);
        let event = tracker.record(1_000_000_000_100, 50, 25);
        assert_eq!(event.bytes_in, 150);
        assert_eq!(event.bytes_out, 75);
    }

    #[test]
    fn empty_row_batch_yields_no_events() {
        let mut tracker = TrafficTracker::new(60);
        assert!(tracker.record_rows(1_000_000_000_000, &[]).is_empty());
    }
}
