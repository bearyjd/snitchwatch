//! Per-`Notice`-kind cooldown gate (Task 17).
//!
//! Ported from `snitchwatch-tauri::notifier`'s `CooldownGate` — pure
//! per-`NoticeKey` cooldown tracking with zero Tauri/D-Bus dependency —
//! and extended in 0d with a per-flow repeat window. The dispatch mechanism (what actually
//! shows a desktop notification) lives in `notification_controller.rs`, the
//! cxx-qt wrapper that owns one `CooldownGate` per live feed task.

use snitchwatch_bridge::notice::Notice;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);
/// Past this many entries, those past the repeat window are pruned.
const MAX_LIVE_FLOWS: usize = 256;
/// How long a posted notice absorbs repeats of its flow.
const DEFAULT_REPEAT_WINDOW: Duration = Duration::from_secs(3600);

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

/// A flow's posted notice, whose repeats coalesce for the repeat window.
#[derive(Debug)]
struct LiveFlow {
    posted_at: Instant,
    suppressed: u32,
}

/// What to do with a notice that may belong to a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowDecision {
    Fire,
    /// A repeat of a posted notice within the repeat window.
    Repeat,
    /// Held back by a cooldown.
    CooledDown,
}

pub struct CooldownGate {
    last_fired: HashMap<NoticeKey, Instant>,
    last_fired_flow: HashMap<FlowKey, Instant>,
    /// Flows with a notice posted within the repeat window. The caller
    /// clears an entry when its notice ends in a way that should let the
    /// next ask notify; the others stay until the window ends.
    live_flows: HashMap<FlowKey, LiveFlow>,
    cooldown: Duration,
    repeat_window: Duration,
}

impl CooldownGate {
    pub fn new() -> Self {
        Self::with_windows(DEFAULT_COOLDOWN, DEFAULT_REPEAT_WINDOW)
    }

    pub fn with_cooldown(cooldown: Duration) -> Self {
        Self::with_windows(cooldown, DEFAULT_REPEAT_WINDOW)
    }

    pub fn with_windows(cooldown: Duration, repeat_window: Duration) -> Self {
        Self {
            last_fired: HashMap::new(),
            last_fired_flow: HashMap::new(),
            live_flows: HashMap::new(),
            cooldown,
            repeat_window,
        }
    }

    /// [`decide_for_flow`](Self::decide_for_flow), as whether to post.
    #[cfg(test)]
    pub fn should_fire_for_flow(
        &mut self,
        notice: &Notice,
        flow: Option<&FlowKey>,
        now: Instant,
    ) -> bool {
        self.decide_for_flow(notice, flow, now) == FlowDecision::Fire
    }

    /// A repeat of a flow's posted notice within the repeat window is
    /// counted and not posted. Otherwise [`should_fire`](Self::should_fire),
    /// and for a notice that has a `flow`, also no second notice for that
    /// flow within the cooldown. A notice held back takes no cooldown and
    /// starts no entry; a repeat only bumps the flow's count.
    pub fn decide_for_flow(
        &mut self,
        notice: &Notice,
        flow: Option<&FlowKey>,
        now: Instant,
    ) -> FlowDecision {
        if let Some(flow) = flow {
            if self.is_repeat(flow, now) {
                return FlowDecision::Repeat;
            }
            let cooled = self
                .last_fired_flow
                .get(flow)
                .is_none_or(|prev| now.duration_since(*prev) >= self.cooldown);
            if !cooled {
                return FlowDecision::CooledDown;
            }
        }
        if !self.should_fire(notice, now) {
            return FlowDecision::CooledDown;
        }
        if let Some(flow) = flow {
            self.last_fired_flow.insert(flow.clone(), now);
            self.prune(now);
            self.live_flows.insert(
                flow.clone(),
                LiveFlow {
                    posted_at: now,
                    suppressed: 0,
                },
            );
        }
        FlowDecision::Fire
    }

    /// Bounds `live_flows`: past [`MAX_LIVE_FLOWS`], entries past the
    /// repeat window go. Unexpired entries are never dropped.
    fn prune(&mut self, now: Instant) {
        if self.live_flows.len() > MAX_LIVE_FLOWS {
            let window = self.repeat_window;
            self.live_flows
                .retain(|_, live| now.duration_since(live.posted_at) < window);
        }
    }

    /// Counts a repeat of a posted flow; an expired live entry is dropped.
    fn is_repeat(&mut self, flow: &FlowKey, now: Instant) -> bool {
        let Some(live) = self.live_flows.get_mut(flow) else {
            return false;
        };
        if now.duration_since(live.posted_at) < self.repeat_window {
            live.suppressed += 1;
            return true;
        }
        self.live_flows.remove(flow);
        false
    }

    /// The flow's notice ended in a way that lets its next ask notify.
    /// The cooldowns stay.
    pub fn clear_flow(&mut self, flow: &FlowKey) {
        self.live_flows.remove(flow);
    }

    /// How many repeats of the flow's posted notice were held back. Read by
    /// the "retried N times" counter (0d part 2).
    pub fn suppressed(&self, flow: &FlowKey) -> u32 {
        self.live_flows.get(flow).map_or(0, |live| live.suppressed)
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
    /// The repeat window equals the cooldown here, so this is the cooldown alone.
    #[test]
    fn retries_of_one_flow_share_a_pending_cooldown() {
        let mut gate = CooldownGate::with_windows(Duration::from_secs(60), Duration::from_secs(60));
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

    fn flow(session: u64, program: &str, host: &str) -> FlowKey {
        FlowKey {
            session,
            program: program.into(),
            host: host.into(),
        }
    }

    fn pending(row_id: u64) -> Notice {
        Notice::Pending {
            row_id,
            process: "curl".into(),
        }
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn the_first_ask_of_a_flow_fires_and_a_repeat_within_the_hour_does_not() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        assert_eq!(
            gate.decide_for_flow(&pending(1), Some(&curl), t0),
            FlowDecision::Fire
        );
        for (row_id, secs) in [(2, 5), (3, 31), (4, 3599)] {
            let at = t0 + Duration::from_secs(secs);
            assert_eq!(
                gate.decide_for_flow(&pending(row_id), Some(&curl), at),
                FlowDecision::Repeat
            );
        }
        assert_eq!(gate.suppressed(&curl), 3);
    }

    #[test]
    fn a_repeat_after_the_hour_fires_again() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        gate.decide_for_flow(&pending(1), Some(&curl), t0);
        gate.decide_for_flow(&pending(2), Some(&curl), t0 + Duration::from_secs(40));
        assert_eq!(
            gate.decide_for_flow(&pending(3), Some(&curl), t0 + HOUR),
            FlowDecision::Fire
        );
        // The expired entry was replaced: a fresh count and a fresh hour.
        assert_eq!(gate.suppressed(&curl), 0);
        assert_eq!(
            gate.decide_for_flow(
                &pending(4),
                Some(&curl),
                t0 + HOUR + Duration::from_secs(60)
            ),
            FlowDecision::Repeat
        );
    }

    #[test]
    fn a_new_host_or_session_fires_under_the_thirty_second_gate_alone() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        gate.decide_for_flow(&pending(1), Some(&curl), t0);
        let later = t0 + Duration::from_secs(5);
        for (row_id, other) in (10..).zip([
            flow(1, "/usr/bin/curl", "example.org"),
            flow(2, "/usr/bin/curl", "example.com"),
        ]) {
            assert_eq!(
                gate.decide_for_flow(&pending(row_id), Some(&other), later),
                FlowDecision::Fire
            );
        }
        // The same host again is a repeat of its live notice, and it
        // does not touch another flow's count.
        assert_eq!(
            gate.decide_for_flow(&pending(2), Some(&curl), later),
            FlowDecision::Repeat
        );
        assert_eq!(gate.suppressed(&curl), 1);
        assert_eq!(gate.suppressed(&flow(1, "/usr/bin/curl", "example.org")), 0);
    }

    #[test]
    fn clearing_a_flow_lets_the_next_ask_fire_past_the_cooldown() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        gate.decide_for_flow(&pending(1), Some(&curl), t0);
        gate.clear_flow(&curl);
        assert_eq!(gate.suppressed(&curl), 0);
        // Within the 30 s gate the cleared flow is held by the cooldown,
        // not suppressed as a repeat.
        let soon = t0 + Duration::from_secs(10);
        assert_eq!(
            gate.decide_for_flow(&pending(2), Some(&curl), soon),
            FlowDecision::CooledDown
        );
        let later = t0 + Duration::from_secs(31);
        assert_eq!(
            gate.decide_for_flow(&pending(3), Some(&curl), later),
            FlowDecision::Fire
        );
    }

    #[test]
    fn a_held_back_notice_records_nothing() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        gate.decide_for_flow(&pending(1), Some(&curl), t0);
        gate.clear_flow(&curl);
        let soon = t0 + Duration::from_secs(10);
        assert_eq!(
            gate.decide_for_flow(&pending(2), Some(&curl), soon),
            FlowDecision::CooledDown
        );
        // No live entry was started by the held-back notice.
        assert_eq!(
            gate.decide_for_flow(&pending(3), Some(&curl), soon),
            FlowDecision::CooledDown
        );
        assert_eq!(gate.suppressed(&curl), 0);
    }

    #[test]
    fn two_flows_are_independent() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let a = flow(1, "/usr/bin/curl", "a.example");
        let b = flow(1, "/usr/bin/curl", "b.example");
        gate.decide_for_flow(&pending(1), Some(&a), t0);
        gate.decide_for_flow(&pending(2), Some(&b), t0);
        gate.clear_flow(&a);
        let later = t0 + Duration::from_secs(40);
        assert_eq!(
            gate.decide_for_flow(&pending(3), Some(&a), later),
            FlowDecision::Fire
        );
        assert_eq!(
            gate.decide_for_flow(&pending(4), Some(&b), later),
            FlowDecision::Repeat
        );
    }

    #[test]
    fn the_repeat_window_is_separate_from_the_cooldown() {
        let mut gate =
            CooldownGate::with_windows(Duration::from_secs(60), Duration::from_secs(120));
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        gate.decide_for_flow(&pending(1), Some(&curl), t0);
        // Past the 60 s cooldown but inside the 120 s window: a repeat.
        assert_eq!(
            gate.decide_for_flow(&pending(2), Some(&curl), t0 + Duration::from_secs(90)),
            FlowDecision::Repeat
        );
        assert_eq!(
            gate.decide_for_flow(&pending(3), Some(&curl), t0 + Duration::from_secs(120)),
            FlowDecision::Fire
        );
    }

    #[test]
    fn a_notice_without_a_flow_never_repeats() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        assert_eq!(
            gate.decide_for_flow(&pending(1), None, t0),
            FlowDecision::Fire
        );
        assert_eq!(
            gate.decide_for_flow(&pending(2), None, t0 + Duration::from_secs(1)),
            FlowDecision::Fire
        );
        assert_eq!(
            gate.decide_for_flow(&pending(1), None, t0 + Duration::from_secs(1)),
            FlowDecision::CooledDown
        );
    }

    /// 0d: a notice that ended on its own (timeout, dismissed) is not
    /// cleared, so the flow stays coalesced for the whole hour.
    #[test]
    fn an_entry_that_is_not_cleared_holds_for_the_hour() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let curl = flow(1, "/usr/bin/curl", "example.com");
        gate.decide_for_flow(&pending(1), Some(&curl), t0);
        let mid = t0 + Duration::from_secs(40 * 60);
        assert_eq!(
            gate.decide_for_flow(&pending(2), Some(&curl), mid),
            FlowDecision::Repeat
        );
        assert_eq!(
            gate.decide_for_flow(&pending(3), Some(&curl), t0 + HOUR),
            FlowDecision::Fire
        );
    }

    #[test]
    fn pruning_drops_expired_entries_only_and_only_past_the_limit() {
        let mut gate = CooldownGate::new();
        let t0 = Instant::now();
        let old = |n: usize| flow(1, "/usr/bin/curl", &format!("old{n}.example"));
        for n in 0..MAX_LIVE_FLOWS {
            gate.decide_for_flow(&pending(n as u64), Some(&old(n)), t0);
        }
        // At the limit nothing is pruned, expired or not.
        let later = t0 + HOUR + Duration::from_secs(1);
        let fresh = flow(1, "/usr/bin/curl", "fresh.example");
        gate.decide_for_flow(&pending(1000), Some(&fresh), later);
        assert_eq!(gate.live_flows.len(), MAX_LIVE_FLOWS + 1);
        // One more past the limit prunes the expired ones; the unexpired stay.
        let newer = flow(1, "/usr/bin/curl", "newer.example");
        gate.decide_for_flow(&pending(1001), Some(&newer), later);
        assert_eq!(gate.live_flows.len(), 2);
        assert!(gate.live_flows.contains_key(&fresh));
        assert!(gate.live_flows.contains_key(&newer));
    }
}
