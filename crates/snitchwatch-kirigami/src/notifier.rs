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
    DenyScopeNarrowedForRow(u64),
    VerdictNotRememberedForRow(u64),
    PromptSlotSummaryForRow(u64),
}

impl From<&Notice> for NoticeKey {
    fn from(notice: &Notice) -> Self {
        match notice {
            Notice::Pending { row_id, .. } => NoticeKey::PendingForRow(*row_id),
            Notice::DaemonAway => NoticeKey::DaemonAway,
            Notice::FilterPauseExpired => NoticeKey::FilterPauseExpired,
            Notice::DenyScopeNarrowed { row_id, .. } => NoticeKey::DenyScopeNarrowedForRow(*row_id),
            Notice::VerdictNotRemembered { row_id } => {
                NoticeKey::VerdictNotRememberedForRow(*row_id)
            }
            Notice::PromptSlotSummary { row_id, .. } => NoticeKey::PromptSlotSummaryForRow(*row_id),
        }
    }
}

/// What a pending prompt is about: the session, the program and the host.
/// A daemon retry makes a new row (and `row_id`) for the same flow (#116 L5),
/// so a Pending notice also has a cooldown of its own per flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlowKey {
    pub session: u64,
    pub program: String,
    pub host: String,
}

pub struct CooldownGate {
    last_fired: HashMap<NoticeKey, Instant>,
    last_fired_flow: HashMap<FlowKey, Instant>,
    cooldown: Duration,
}

impl CooldownGate {
    pub fn new() -> Self {
        Self {
            last_fired: HashMap::new(),
            last_fired_flow: HashMap::new(),
            cooldown: DEFAULT_COOLDOWN,
        }
    }

    pub fn with_cooldown(cooldown: Duration) -> Self {
        Self {
            last_fired: HashMap::new(),
            last_fired_flow: HashMap::new(),
            cooldown,
        }
    }

    /// [`should_fire`](Self::should_fire), and for a notice that has a
    /// `flow`, also no second notice for that flow within the cooldown. A
    /// notice held back by either takes neither.
    pub fn should_fire_for_flow(
        &mut self,
        notice: &Notice,
        flow: Option<&FlowKey>,
        now: Instant,
    ) -> bool {
        let cooled = |last: Option<&Instant>| {
            last.is_none_or(|prev| now.duration_since(*prev) >= self.cooldown)
        };
        if let Some(flow) = flow {
            if !cooled(self.last_fired_flow.get(flow)) {
                return false;
            }
        }
        if !self.should_fire(notice, now) {
            return false;
        }
        if let Some(flow) = flow {
            self.last_fired_flow.insert(flow.clone(), now);
        }
        true
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

    /// #116 L5: daemon retries are new rows for one flow; they share a notice.
    #[test]
    fn retries_of_one_flow_share_a_pending_cooldown() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let t0 = Instant::now();
        let pending = |row_id| Notice::Pending {
            row_id,
            process: "curl".into(),
        };
        let flow = |session, program: &str, host: &str| FlowKey {
            session,
            program: program.into(),
            host: host.into(),
        };
        let curl = flow(1, "/usr/bin/curl", "example.com");
        assert!(gate.should_fire_for_flow(&pending(1), Some(&curl), t0));
        let later = t0 + Duration::from_secs(5);
        assert!(!gate.should_fire_for_flow(&pending(2), Some(&curl), later));
        // Another host, program or session is another flow.
        for (row_id, other) in (10..).zip([
            flow(1, "/usr/bin/curl", "example.org"),
            flow(1, "/usr/bin/wget", "example.com"),
            flow(2, "/usr/bin/curl", "example.com"),
        ]) {
            assert!(gate.should_fire_for_flow(&pending(row_id), Some(&other), later));
        }
        // The held-back retry took no cooldown of its own: row 2 is still
        // free to fire once the flow's window ends.
        assert!(gate.should_fire_for_flow(&pending(2), Some(&curl), t0 + Duration::from_secs(61)));
        // Without a flow it is the per-row cooldown alone.
        assert!(gate.should_fire_for_flow(&pending(99), None, later));
    }

    /// Issue #44: each answered row gets its own notice.
    #[test]
    fn verdict_not_remembered_cooldown_is_per_row() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let t0 = Instant::now();
        let row_a = Notice::VerdictNotRemembered { row_id: 1 };
        let row_b = Notice::VerdictNotRemembered { row_id: 2 };
        assert!(gate.should_fire(&row_a, t0));
        assert!(gate.should_fire(&row_b, t0));
        assert!(!gate.should_fire(&row_a, t0 + Duration::from_secs(5)));
        assert!(gate.should_fire(
            &Notice::DenyScopeNarrowed {
                row_id: 1,
                what: String::new(),
                reason: String::new(),
            },
            t0
        ));
    }

    /// Prompt-slot plan, part A: one summary per released prompt.
    #[test]
    fn prompt_slot_summary_cooldown_is_per_prompt() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let t0 = Instant::now();
        let first = Notice::PromptSlotSummary {
            row_id: 1,
            count: 4,
        };
        let second = Notice::PromptSlotSummary {
            row_id: 2,
            count: 1,
        };
        assert!(gate.should_fire(&first, t0));
        assert!(gate.should_fire(&second, t0));
        assert!(!gate.should_fire(&first, t0 + Duration::from_secs(5)));
    }
}
