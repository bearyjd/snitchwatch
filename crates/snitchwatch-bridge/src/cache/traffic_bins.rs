//! Bin per-connection byte counters into uPlot-compatible time buckets.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub struct TrafficSample {
    pub timestamp_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// Fixed-size ring buffer of 1-second buckets.
#[derive(Debug)]
pub struct TrafficBinner {
    buckets: VecDeque<TrafficSample>,
    bucket_ms: i64,
    capacity: usize,
}

impl TrafficBinner {
    pub fn new(window_seconds: usize) -> Self {
        Self {
            buckets: VecDeque::with_capacity(window_seconds),
            bucket_ms: 1000,
            capacity: window_seconds,
        }
    }

    pub fn record(&mut self, timestamp_ms: i64, bytes_in: u64, bytes_out: u64) {
        // Round timestamp down to bucket boundary
        let bucket_ts = (timestamp_ms / self.bucket_ms) * self.bucket_ms;

        // Fast path: same bucket as latest
        if let Some(latest) = self.buckets.back_mut() {
            if latest.timestamp_ms == bucket_ts {
                latest.bytes_in = latest.bytes_in.saturating_add(bytes_in);
                latest.bytes_out = latest.bytes_out.saturating_add(bytes_out);
                return;
            }
            if bucket_ts < latest.timestamp_ms {
                // Out-of-order sample — find or create the right bucket.
                // For simplicity in v1, we drop out-of-order samples and log.
                tracing::debug!(
                    bucket_ts,
                    latest_ts = latest.timestamp_ms,
                    "dropped out-of-order traffic sample"
                );
                return;
            }
        }

        // Fill any gap buckets so the chart shows zeros instead of holes
        if let Some(latest) = self.buckets.back() {
            let mut next_ts = latest.timestamp_ms + self.bucket_ms;
            while next_ts < bucket_ts {
                self.push(TrafficSample {
                    timestamp_ms: next_ts,
                    bytes_in: 0,
                    bytes_out: 0,
                });
                next_ts += self.bucket_ms;
            }
        }

        self.push(TrafficSample {
            timestamp_ms: bucket_ts,
            bytes_in,
            bytes_out,
        });
    }

    fn push(&mut self, sample: TrafficSample) {
        if self.buckets.len() == self.capacity {
            self.buckets.pop_front();
        }
        self.buckets.push_back(sample);
    }

    /// Return the buckets in uPlot format: (timestamps, bytes_in_series, bytes_out_series).
    pub fn series(&self) -> (Vec<i64>, Vec<u64>, Vec<u64>) {
        let mut ts = Vec::with_capacity(self.buckets.len());
        let mut bin = Vec::with_capacity(self.buckets.len());
        let mut bout = Vec::with_capacity(self.buckets.len());
        for s in &self.buckets {
            ts.push(s.timestamp_ms);
            bin.push(s.bytes_in);
            bout.push(s.bytes_out);
        }
        (ts, bin, bout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_in_same_second_aggregate() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_000_000, 100, 50);
        b.record(1_000_000_000_500, 200, 25);
        let (ts, bi, bo) = b.series();
        assert_eq!(ts, vec![1_000_000_000_000]);
        assert_eq!(bi, vec![300]);
        assert_eq!(bo, vec![75]);
    }

    #[test]
    fn samples_in_different_seconds_make_new_buckets() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_000_000, 100, 50);
        b.record(1_000_000_001_000, 200, 25);
        let (ts, bi, _) = b.series();
        assert_eq!(ts.len(), 2);
        assert_eq!(bi, vec![100, 200]);
    }

    #[test]
    fn gaps_are_filled_with_zero_buckets() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_000_000, 100, 0);
        b.record(1_000_000_003_000, 50, 0);
        let (ts, bi, _) = b.series();
        assert_eq!(ts.len(), 4, "must fill the 2-second gap: {:?}", ts);
        assert_eq!(bi, vec![100, 0, 0, 50]);
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut b = TrafficBinner::new(3);
        for i in 0..5 {
            b.record(1_000_000_000_000 + i * 1000, i as u64, 0);
        }
        let (ts, _, _) = b.series();
        assert_eq!(ts.len(), 3);
        assert_eq!(
            ts,
            vec![1_000_000_002_000, 1_000_000_003_000, 1_000_000_004_000]
        );
    }

    #[test]
    fn out_of_order_samples_are_dropped() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_005_000, 100, 0);
        b.record(1_000_000_002_000, 999, 0); // older — dropped
        let (_, bi, _) = b.series();
        assert!(!bi.contains(&999));
    }
}
