//! A persistent disagreement between the daemon's rule count and the
//! bridge's list (issue #65, option c; plan
//! `docs/superpowers/plans/2026-10-09-rules-count-hint-65.md`).

use super::{lock, RulesCache, RulesSync};
use std::time::Instant;
use tracing::info;

/// Readings ignored after the bridge changed its own list or the daemon
/// restarted: a ping built before the change is at most one reading old.
pub(crate) const QUIET_PINGS: u8 = 2;
/// Readings in a row that must repeat one disagreement before the hint shows.
pub(crate) const PINGS_TO_RAISE: u8 = 3;
/// Readings in a row that must agree before the hint goes.
pub(crate) const PINGS_TO_CLEAR: u8 = 3;

/// The debounce state; only `raised` leaves the cache.
#[derive(Debug, Clone, Default)]
pub(super) struct CountWatch {
    /// The cache revision at the last reading: a different one means the
    /// bridge changed its own list since.
    seen_revision: u64,
    last_uptime: Option<u64>,
    /// Readings still to ignore.
    quiet: u8,
    /// The `(reported, expected)` pair the current run repeats.
    key: Option<(u64, usize)>,
    run: u8,
    raised: bool,
}

impl CountWatch {
    fn restart_run(&mut self) {
        self.key = None;
        self.run = 0;
    }
}

/// One ping's worth of evidence.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reading {
    /// `Statistics.rules`: the daemon's loaded-rule count.
    pub(crate) reported: u64,
    /// `Statistics.uptime`; a fall means another daemon process.
    pub(crate) uptime: u64,
    /// A command waits for its reply or a snapshot for its HELLO: the daemon
    /// and the list are moving apart on purpose.
    pub(crate) settling: bool,
}

/// The daemon starts a timer for a rule whose duration is none of these
/// (`vendor:daemon/rule/loader.go` `isTemporary`).
pub(super) fn is_temporary(duration: &str) -> bool {
    !matches!(duration, "once" | "until restart" | "always")
}

impl RulesCache {
    /// Whether the hint is on.
    pub fn count_mismatch(&self) -> bool {
        self.count_watch.raised
    }

    pub(crate) fn note_left_out_temporary(&mut self, temporary: bool) {
        self.left_out_temporary = temporary;
    }

    /// The rules the daemon should hold: the list and what it left out.
    fn expected_count(&self) -> Option<usize> {
        self.rules
            .as_ref()
            .map(|rules| rules.len() + self.left_out.len())
    }

    /// A listed or left-out rule whose daemon timer may fire unseen: the
    /// timer outlives a later disable, a snapshot gives a disabled rule no
    /// expiry, and the bridge cannot parse every duration the daemon can.
    fn timers_may_drop_rules(&self) -> bool {
        self.left_out_temporary
            || self
                .rules
                .as_ref()
                .is_some_and(|rules| rules.values().any(|rule| is_temporary(&rule.duration)))
    }

    /// Take one ping's count and say whether the hint flipped. Needs
    /// [`PINGS_TO_RAISE`] readings in a row that repeat one disagreement to
    /// raise it, and [`PINGS_TO_CLEAR`] that agree to clear it; nothing else
    /// moves it (hysteresis). Readings are ignored right after a change of
    /// the list or a daemon restart ([`QUIET_PINGS`]), and are not evidence
    /// while the count cannot be compared (see the plan's table).
    pub(crate) fn observe_rule_count(&mut self, reading: Reading) -> bool {
        let Some(expected) = self.expected_count() else {
            return false;
        };
        let cannot_compare = reading.settling
            || self.timers_may_drop_rules()
            // A proto3 scalar: 0 may be "not reported".
            || (reading.reported == 0 && expected > 0);
        let revision = self.revision;
        let watch = &mut self.count_watch;
        let restarted = watch
            .last_uptime
            .is_some_and(|before| reading.uptime < before);
        watch.last_uptime = Some(reading.uptime);
        if watch.seen_revision != revision || restarted {
            watch.seen_revision = revision;
            watch.quiet = QUIET_PINGS;
            watch.restart_run();
        }
        if watch.quiet > 0 {
            watch.quiet -= 1;
            return false;
        }
        if cannot_compare {
            watch.restart_run();
            return false;
        }
        let key = (reading.reported, expected);
        if watch.key == Some(key) {
            watch.run = watch.run.saturating_add(1);
        } else {
            watch.key = Some(key);
            watch.run = 1;
        }
        let agrees = usize::try_from(reading.reported).is_ok_and(|reported| reported == expected);
        match (watch.raised, agrees) {
            (false, false) if watch.run >= PINGS_TO_RAISE => watch.raised = true,
            (true, true) if watch.run >= PINGS_TO_CLEAR => watch.raised = false,
            _ => return false,
        }
        true
    }
}

impl RulesSync {
    /// A ping's `Statistics.rules` and `uptime`, with how many rule commands
    /// still wait for the daemon's reply. Broadcasts a `RulesNotShown` (no
    /// list) only when the hint flips, with the cache lock held like every
    /// list publisher. The waiting snapshots are read, and the lock
    /// released, before the cache lock is taken.
    pub fn observe_daemon_rules(&self, reported: u64, uptime: u64, commands_in_flight: usize) {
        let snapshot_waits = lock(&self.pending).awaiting_adoption(Instant::now());
        let reading = Reading {
            reported,
            uptime,
            settling: snapshot_waits || commands_in_flight > 0,
        };
        let mut cache = lock(&self.cache);
        if cache.observe_rule_count(reading) {
            info!(
                reported,
                expected = cache.expected_count(),
                shown = cache.count_mismatch(),
                revision = cache.revision(),
                "the rules-count hint changed"
            );
            let _ = self.broadcast.send(cache.not_shown());
        }
    }
}

#[cfg(test)]
#[path = "rules_count_tests.rs"]
mod tests;
