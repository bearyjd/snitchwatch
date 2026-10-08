//! Refusal backoff (issue #73): a list the daemon refuses is not retried on
//! every 15-minute refresh tick, each retry re-reading the whole list from the
//! store. After the first refusal a tick leaves it alone for 15 minutes, after
//! the second for an hour, and after every one after that for four hours. A
//! new daemon rule list or a new download is tried at once, whatever the
//! schedule says, and a success starts the schedule over; a daemon that
//! doesn't answer is not a refusal and is retried every tick. The reason
//! shown to the user says when the next try is.
//!
//! The schedule lives in memory and runs on the monotonic clock (tests pause
//! it): a restart forgets it, so a list the daemon refuses is tried at the
//! first pass of the new run and the schedule starts again from there.

use std::time::Duration;

use tokio::time::Instant;

use super::BlocklistsManager;
use crate::blocklists::worker::DEFAULT_REFRESH_TICK;
use crate::blocklists::{NotInstalled, NO_HOSTS_REASON, REFUSAL_BACKOFF_MINUTES};

/// A refresh tick comes every 15 minutes, counted from its own start, but a
/// refusal is stamped when its install finishes, which can be well after: the
/// tick starts with the downloads, and a large list takes minutes. Without
/// slack the tick exactly one period later would find the refusal that much
/// short of its time and skip it, so the schedule would run a whole tick
/// long. Within half a tick of its time a list is due. (A pass whose
/// downloads take longer than that can still run a tick long.)
const TICK_SLACK: Duration = Duration::from_secs(DEFAULT_REFRESH_TICK.as_secs() / 2);

/// How often a list was refused in a row, and when a tick may try again.
#[derive(Debug, Clone, Copy)]
pub(super) struct Refusal {
    failures: u32,
    retry_after: Instant,
}

impl BlocklistsManager {
    fn refusals(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Refusal>> {
        self.refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether a refresh tick should leave `id` alone for now.
    pub(super) fn backing_off(&self, id: &str) -> bool {
        self.refusals()
            .get(id)
            .is_some_and(|refusal| Instant::now() + TICK_SLACK < refusal.retry_after)
    }

    pub(super) fn forget_refusals(&self, id: &str) {
        self.refusals().remove(id);
    }

    #[cfg(test)]
    pub(crate) fn refusal_state(&self, id: &str) -> Option<(u32, Instant)> {
        self.refusals()
            .get(id)
            .map(|refusal| (refusal.failures, refusal.retry_after))
    }

    /// Note how an install went. A success clears the schedule; a refusal
    /// advances it and says when the next try is; a daemon that didn't answer
    /// changes nothing.
    pub(super) fn track_install(
        &self,
        id: &str,
        outcome: Result<(), NotInstalled>,
    ) -> Result<(), NotInstalled> {
        let refused = match outcome {
            Ok(()) => {
                self.forget_refusals(id);
                return Ok(());
            }
            Err(e) if e.daemon_unavailable || e.cleanup_pending => return Err(e),
            Err(e) => e,
        };
        if refused.reason == NO_HOSTS_REASON {
            // Retrying changes nothing; the next download does.
            return Err(refused);
        }
        let mut refusals = self.refusals();
        let failures = refusals.get(id).map_or(0, |r| r.failures).saturating_add(1);
        let minutes = backoff_minutes(failures);
        refusals.insert(
            id.to_string(),
            Refusal {
                failures,
                retry_after: Instant::now() + Duration::from_secs(minutes.unsigned_abs() * 60),
            },
        );
        Err(NotInstalled {
            reason: format!(
                "{} Snitchwatch will try again in about {}.",
                sentence(&refused.reason),
                how_long(minutes)
            ),
            daemon_unavailable: false,
            cleanup_pending: false,
        })
    }
}

fn backoff_minutes(failures: u32) -> i64 {
    let step = usize::try_from(failures.saturating_sub(1)).unwrap_or(usize::MAX);
    REFUSAL_BACKOFF_MINUTES[step.min(REFUSAL_BACKOFF_MINUTES.len() - 1)]
}

fn how_long(minutes: i64) -> String {
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    if minutes % 60 == 0 {
        plural(minutes / 60, "hour")
    } else {
        plural(minutes, "minute")
    }
}

/// `reason` ending in one full stop.
fn sentence(reason: &str) -> String {
    format!("{}.", reason.trim_end_matches(['.', ' ']))
}
