//! Desktop notification dispatcher.
//!
//! Subscribes to NoticeBus and forwards each Notice to notify-rust. Applies a
//! per-kind cooldown (default 30s) so we don't spam the user when the bridge
//! republishes Pending five times for the same row.

use snitchwatch_bridge::notice::Notice;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, Hash)]
enum NoticeKey {
    PendingForRow(u64),
    DaemonAway,
    FilterPauseExpired,
    DenyScopeNarrowedForRow(u64),
}

impl From<&Notice> for NoticeKey {
    fn from(notice: &Notice) -> Self {
        match notice {
            Notice::Pending { row_id, .. } => NoticeKey::PendingForRow(*row_id),
            Notice::DaemonAway => NoticeKey::DaemonAway,
            Notice::FilterPauseExpired => NoticeKey::FilterPauseExpired,
            Notice::DenyScopeNarrowed { row_id, .. } => NoticeKey::DenyScopeNarrowedForRow(*row_id),
        }
    }
}

pub struct CooldownGate {
    last_fired: HashMap<NoticeKey, Instant>,
    cooldown: Duration,
}

impl CooldownGate {
    pub fn new() -> Self {
        Self {
            last_fired: HashMap::new(),
            cooldown: DEFAULT_COOLDOWN,
        }
    }

    pub fn with_cooldown(cooldown: Duration) -> Self {
        Self {
            last_fired: HashMap::new(),
            cooldown,
        }
    }

    pub fn should_fire(&mut self, notice: &Notice, now: Instant) -> bool {
        let key = NoticeKey::from(notice);
        let allow = match self.last_fired.get(&key) {
            Some(prev) => now.duration_since(*prev) >= self.cooldown,
            None => true,
        };
        if allow {
            self.last_fired.insert(key, now);
        }
        allow
    }
}

impl Default for CooldownGate {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Notifier {
    gate: CooldownGate,
}

impl Notifier {
    pub fn new() -> Self {
        Self {
            gate: CooldownGate::new(),
        }
    }

    pub async fn run(mut self, mut rx: broadcast::Receiver<Notice>) {
        while let Ok(notice) = rx.recv().await {
            if !self.gate.should_fire(&notice, Instant::now()) {
                continue;
            }
            self.dispatch(&notice);
        }
    }

    fn dispatch(&self, notice: &Notice) {
        let (summary, body) = match notice {
            Notice::Pending { process, .. } => (
                "Snitchwatch — pending decision",
                format!("{process} is asking to connect"),
            ),
            Notice::DaemonAway => (
                "Snitchwatch — daemon unreachable",
                "opensnitchd has been unreachable for 30 seconds.".into(),
            ),
            Notice::FilterPauseExpired => (
                "Snitchwatch — filtering resumed",
                "Your pause timer expired.".into(),
            ),
            Notice::DenyScopeNarrowed { what, reason, .. } => (
                "Snitchwatch — block narrowed",
                format!("Blocked {what} for this host only — {reason}."),
            ),
        };
        if let Err(err) = notify_rust::Notification::new()
            .summary(summary)
            .body(&body)
            .icon("snitchwatch")
            .show()
        {
            tracing::warn!(?err, "failed to dispatch desktop notification");
        }
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_blocks_repeat_within_window() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let n = Notice::DaemonAway;
        let t0 = Instant::now();
        assert!(gate.should_fire(&n, t0));
        assert!(!gate.should_fire(&n, t0 + Duration::from_secs(10)));
        assert!(!gate.should_fire(&n, t0 + Duration::from_secs(59)));
    }

    #[test]
    fn cooldown_allows_repeat_after_window() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let n = Notice::DaemonAway;
        let t0 = Instant::now();
        assert!(gate.should_fire(&n, t0));
        assert!(gate.should_fire(&n, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn distinct_pending_rows_have_independent_cooldowns() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let t0 = Instant::now();
        let row_a = Notice::Pending {
            row_id: 1,
            process: "firefox".into(),
        };
        let row_b = Notice::Pending {
            row_id: 2,
            process: "slack".into(),
        };
        assert!(gate.should_fire(&row_a, t0));
        assert!(gate.should_fire(&row_b, t0));
        assert!(!gate.should_fire(&row_a, t0 + Duration::from_secs(5)));
    }
}
