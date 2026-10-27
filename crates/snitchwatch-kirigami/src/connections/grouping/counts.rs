//! Per-group aggregate counts, kept incrementally by [`super::GroupTree`].

use super::super::row_store::Verdict;
use super::windows::WindowFigures;

/// Running aggregate counts for a group (process or domain level).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupCounts {
    pub total: usize,
    pub pending: usize,
    pub allowed: usize,
    pub denied: usize,
    pub blocklisted: usize,
    pub other: usize,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// The last hour's and today's figures. Not kept by `add`/`sub`: a
    /// projection fills them in from the group's window buckets.
    pub windows: WindowFigures,
}

impl GroupCounts {
    pub(super) fn add(&mut self, verdict: Verdict, bytes_sent: u64, bytes_received: u64) {
        self.total += 1;
        self.bytes_sent += bytes_sent;
        self.bytes_received += bytes_received;
        match verdict {
            Verdict::Pending => self.pending += 1,
            Verdict::Allowed => self.allowed += 1,
            Verdict::Denied => self.denied += 1,
            Verdict::Blocklisted => self.blocklisted += 1,
            Verdict::Other => self.other += 1,
        }
    }

    pub(super) fn sub(&mut self, verdict: Verdict, bytes_sent: u64, bytes_received: u64) {
        self.total = self.total.saturating_sub(1);
        self.bytes_sent = self.bytes_sent.saturating_sub(bytes_sent);
        self.bytes_received = self.bytes_received.saturating_sub(bytes_received);
        match verdict {
            Verdict::Pending => self.pending = self.pending.saturating_sub(1),
            Verdict::Allowed => self.allowed = self.allowed.saturating_sub(1),
            Verdict::Denied => self.denied = self.denied.saturating_sub(1),
            Verdict::Blocklisted => self.blocklisted = self.blocklisted.saturating_sub(1),
            Verdict::Other => self.other = self.other.saturating_sub(1),
        }
    }
}
