//! Per-`Notice`-kind cooldown gate (Task 17).
//!
//! Ported unchanged from `snitchwatch-tauri::notifier`'s `CooldownGate` —
//! pure per-`NoticeKey` cooldown tracking with zero Tauri/D-Bus dependency.
//! Its 3 unit tests transfer unchanged. The dispatch mechanism (what actually
//! shows a desktop notification) lives in `notification_controller.rs`, the
//! cxx-qt wrapper that owns one `CooldownGate` per live feed task.

use snitchwatch_bridge::notice::Notice;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, Hash)]
enum NoticeKey {
    PendingForRow(u64),
    DaemonAway,
    FilterPauseExpired,
}

impl From<&Notice> for NoticeKey {
    fn from(notice: &Notice) -> Self {
        match notice {
            Notice::Pending { row_id, .. } => NoticeKey::PendingForRow(*row_id),
            Notice::DaemonAway => NoticeKey::DaemonAway,
            Notice::FilterPauseExpired => NoticeKey::FilterPauseExpired,
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
