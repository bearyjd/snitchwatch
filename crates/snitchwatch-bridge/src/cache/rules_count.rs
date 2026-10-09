//! A persistent disagreement between the daemon's rule count and the
//! bridge's list (issue #65, option c; plan
//! `docs/superpowers/plans/2026-10-09-rules-count-hint-65.md`).

use super::{lock, RulesCache, RulesSync};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Readings ignored after the bridge changed its own list or the daemon
/// restarted: a ping built before the change is at most one reading old.
pub(crate) const QUIET_PINGS: u8 = 2;
/// Readings in a row that must repeat one disagreement before the hint shows.
pub(crate) const PINGS_TO_RAISE: u8 = 3;
/// Readings in a row that must agree before the hint goes.
pub(crate) const PINGS_TO_CLEAR: u8 = 3;

/// Most refused-add names [`MayHold`] remembers; later ones aren't noted.
pub(crate) const MAX_MAYBE_APPLIED: usize = 256;
/// How long past its monotonic end a pruned temporary rule may still be in
/// the daemon: the two timers started a round trip apart.
const PRUNED_SLACK: Duration = Duration::from_secs(5);

/// Rules the daemon may hold that the list does not show, each a reason to
/// accept a count above the list's (`expected ..= expected + allowance`).
/// Never a reason to accept a count below it. Cleared with every list.
/// Each is a case where the bridge applies its own change differently from
/// the daemon, and the daemon stays one rule ahead:
/// - **A prompt answer under a listed name.** The daemon stores a prompt
///   answer through `addUserRule` → `setUniqueName` (`loader.go`): `<name>-2`
///   when `<name>` is in memory, as it is for a rule the user turned off
///   (only enabled rules match, so the same program asks again). The bridge
///   replaces `<name>` in its list. Each such answer is one more rule, for
///   good; the next `Subscribe` shows them.
/// - **A refused add.** `Replace` stores a rule in memory before `Save` can
///   fail, and `scheduleTemporaryRule` fails on a duration Go can't parse
///   after the rule is stored; both answer `ERROR`. The bridge can't tell
///   which of its adds went in, so each name that isn't listed may have.
/// - **A temporary rule pruned early.** The bridge prunes by the wall clock;
///   the daemon's timer (`time.AfterFunc`) runs on the monotonic clock,
///   which stops while the host is suspended. Until that timer would have
///   fired (its [`Expiry`](super::Expiry)`::ends`, plus a little) the daemon
///   may still hold the rule.
#[derive(Debug, Clone, Default)]
pub(super) struct MayHold {
    renamed: u32,
    refused: BTreeSet<String>,
    pruned_until: Vec<Instant>,
}

impl MayHold {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// `name` is listed now: the daemon's copy is the list's.
    pub(super) fn forget(&mut self, name: &str) {
        self.refused.remove(name);
    }

    pub(super) fn note_pruned(&mut self, ends: Instant, clock: Instant) {
        self.pruned_until.retain(|until| *until > clock);
        if ends > clock {
            self.pruned_until.push(ends + PRUNED_SLACK);
        }
    }

    /// How many more rules the daemon may hold, at `now`.
    fn allowance(&self, now: Instant) -> usize {
        let renamed = usize::try_from(self.renamed).unwrap_or(usize::MAX);
        let pruned = self
            .pruned_until
            .iter()
            .filter(|until| **until > now)
            .count();
        renamed
            .saturating_add(self.refused.len())
            .saturating_add(pruned)
    }
}

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
    key: Option<(u64, usize, usize)>,
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
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reading {
    /// `Statistics.rules`: the daemon's loaded-rule count.
    pub(crate) reported: u64,
    /// `Statistics.uptime`; a fall means another daemon process.
    pub(crate) uptime: u64,
    /// A command waits for its reply or a snapshot for its HELLO: the daemon
    /// and the list are moving apart on purpose.
    pub(crate) settling: bool,
    /// The monotonic clock, for [`MayHold`].
    pub(crate) now: Instant,
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

    /// A prompt answer for `name` is about to be stored in the list: if the
    /// name is listed, the daemon stores it under another (see [`MayHold`]).
    pub(crate) fn note_prompt_answer(&mut self, name: &str) {
        if self.contains(name) {
            self.may_hold.renamed = self.may_hold.renamed.saturating_add(1);
        }
    }

    /// A command for `name` was refused, and `name` isn't listed: the daemon
    /// may hold it (see [`MayHold`]).
    pub(crate) fn note_maybe_applied(&mut self, name: &str) {
        if self.is_unknown() || self.contains(name) || self.may_hold.refused.contains(name) {
            return;
        }
        if self.may_hold.refused.len() >= MAX_MAYBE_APPLIED {
            warn!("too many refused adds the daemon may hold; not noting another");
            return;
        }
        self.may_hold.refused.insert(name.to_string());
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
            // A proto3 scalar: 0 may be "not reported".
            || (reading.reported == 0 && expected > 0);
        // A hint can still go while a timer may drop rules, never come.
        let may_only_clear = self.timers_may_drop_rules();
        let allowance = self.may_hold.allowance(reading.now);
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
        if cannot_compare || (may_only_clear && !watch.raised) {
            watch.restart_run();
            return false;
        }
        let key = (reading.reported, expected, allowance);
        if watch.key == Some(key) {
            watch.run = watch.run.saturating_add(1);
        } else {
            watch.key = Some(key);
            watch.run = 1;
        }
        let agrees = usize::try_from(reading.reported).is_ok_and(|reported| {
            (expected..=expected.saturating_add(allowance)).contains(&reported)
        });
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
            now: Instant::now(),
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

#[cfg(test)]
#[path = "rules_count_allowance_tests.rs"]
mod allowance_tests;
