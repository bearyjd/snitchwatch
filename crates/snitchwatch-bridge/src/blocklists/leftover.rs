//! Blocklist rules Snitchwatch made that nothing is managing any more
//! (issue #73).
//!
//! A bridge that once installed blocklist rules can lose the means to manage
//! them: its state directory disappears or fails the mode check (it then runs
//! with a [`NoopRuleSink`](crate::blocklists::NoopRuleSink), or no list
//! directory at all), a per-user bridge connects to a daemon that still holds
//! a system bridge's rules, or the saved subscriptions can't be read, so what
//! should stay is unknown and nothing is ever removed on its own. The rules
//! keep blocking their lists' hosts, the Rules page refuses to touch them
//! ("managed on the Blocklists page"), and the Blocklists page shows none.
//!
//! [`LeftoverRules`] reads them from the daemon's rule snapshot and, only when
//! the user asks, deletes them. Only rules Snitchwatch made are ever listed or
//! deleted ([`made_by_bridge`]: the `lists` deny shape, or the legacy blocklist
//! tag); a rule under a blocklist name that someone else made is left alone.
//! A delete names no path, so it goes out without a pinned list root. It goes
//! only to the daemon stream the snapshot came from, and never over the
//! legacy TCP connection, where any local process can pose as the daemon and
//! show a snapshot of its own (see [`DaemonCommands::send_leftover_delete`]);
//! there nothing is listed, so nothing is offered.

use std::time::Duration;

use tracing::warn;

use crate::blocklists::daemon_sink::{made_by_bridge, BLOCKLIST_COMMAND_TIMEOUT};
use crate::blocklists::NotInstalled;
use crate::cache::rules::SharedRulesCache;
use crate::daemon_commands::{
    BlocklistCommand, CommandError, DaemonCommands, DaemonTransport, SendError,
};
use crate::rule_name::is_reserved_blocklist_name;

/// What a removal pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedLeftovers {
    /// Rules the daemon confirmed deleting.
    pub removed: usize,
    /// Rules the daemon refused to delete (logged; they stay listed).
    pub refused: usize,
    /// Rules there were to delete when the pass began.
    pub total: usize,
    /// Why the pass stopped before the end (the daemon couldn't be reached or
    /// didn't answer); the rules after that point were not tried.
    pub stopped: Option<NotInstalled>,
}

pub struct LeftoverRules {
    commands: DaemonCommands,
    rules: SharedRulesCache,
    timeout: Duration,
}

impl LeftoverRules {
    pub fn new(commands: DaemonCommands, rules: SharedRulesCache) -> Self {
        Self {
            commands,
            rules,
            timeout: BLOCKLIST_COMMAND_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Names of the blocklist rules Snitchwatch made in the daemon's
    /// committed rule snapshot, sorted, that it could delete; `None` while
    /// that is unknown, and over the legacy TCP connection.
    pub fn names(&self) -> Option<Vec<String>> {
        if self.commands.transport() == DaemonTransport::Tcp {
            return None;
        }
        let cache = self
            .rules
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.rules().map(|rules| {
            rules
                .values()
                .filter(|rule| is_reserved_blocklist_name(&rule.name) && made_by_bridge(rule))
                .filter(|rule| BlocklistCommand::delete(&rule.name).is_some())
                .map(|rule| rule.name.clone())
                .collect()
        })
    }

    /// Delete every rule [`names`](Self::names) lists, one at a time. A
    /// refusal is counted and skipped; at the first delete the daemon can't be
    /// reached for the pass stops and says why, with what it had done. Only
    /// when the list is unknown does it do nothing and return an error.
    pub async fn remove_all(&self) -> Result<RemovedLeftovers, NotInstalled> {
        let names = self.names().ok_or_else(|| {
            NotInstalled::daemon_unavailable(
                "The firewall service's rule list isn't known yet, so Snitchwatch can't tell \
                 which rules to remove",
            )
        })?;
        let mut outcome = RemovedLeftovers {
            removed: 0,
            refused: 0,
            total: names.len(),
            stopped: None,
        };
        for name in names {
            let Some(command) = BlocklistCommand::delete(&name) else {
                continue;
            };
            match self.delete_one(command).await {
                Ok(()) => outcome.removed += 1,
                Err(DeleteStop::Refused) => {
                    warn!("daemon refused to delete a leftover blocklist rule");
                    outcome.refused += 1;
                }
                Err(DeleteStop::Unreachable(why)) => {
                    outcome.stopped = Some(why);
                    break;
                }
            }
        }
        Ok(outcome)
    }

    async fn delete_one(&self, command: BlocklistCommand) -> Result<(), DeleteStop> {
        let pending = self
            .commands
            .send_leftover_delete(command)
            .map_err(|e| match e {
                SendError::NoDaemon | SendError::NotQueued => DeleteStop::Unreachable(
                    NotInstalled::daemon_unavailable("The firewall service isn't connected"),
                ),
                other => DeleteStop::Unreachable(NotInstalled::new(format!(
                    "Snitchwatch refused to send ({other})"
                ))),
            })?;
        match pending.wait(self.timeout).await {
            Ok(()) => Ok(()),
            Err(CommandError::Rejected(_)) => Err(DeleteStop::Refused),
            Err(CommandError::Timeout | CommandError::StreamClosed) => {
                Err(DeleteStop::Unreachable(NotInstalled::daemon_unavailable(
                    "The firewall service didn't answer",
                )))
            }
        }
    }
}

/// Why one delete didn't end in an `OK`.
enum DeleteStop {
    /// The daemon answered `ERROR`; the pass goes on.
    Refused,
    /// Nothing more can be tried.
    Unreachable(NotInstalled),
}

#[cfg(test)]
#[path = "leftover_tests.rs"]
mod tests;
