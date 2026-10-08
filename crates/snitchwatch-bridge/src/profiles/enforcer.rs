//! Installing the active profile's rules (issue #46 Part 2).
//!
//! A [`ProfileRuleSink`] makes the daemon hold exactly the rules it is
//! given, the active profile's rules that passed the `ProfileRule` policy,
//! and none of the bridge's other profile rules. [`DaemonProfileSink`] does
//! it through [`DaemonCommands::send_profile`], as #45 does for blocklists:
//! 1. nothing is sent while the daemon's rule list is unknown; the pass after
//!    its next committed snapshot does it;
//! 2. a rule already in the committed snapshot, as the daemon echoes it, is
//!    not resent, so a pass after every snapshot sends nothing new;
//! 3. any other rule is sent with `CHANGE_RULE` and counts as installed only
//!    after the daemon's correlated `OK`; a refusal keeps the daemon's text,
//!    and a daemon that can't be reached stops the pass;
//! 4. then cached rules under the profile prefix that aren't wanted are
//!    deleted, but only ones the bridge made
//!    ([`made_by_bridge`]: the prefix and the profile tag).
//!
//! Wanted rules are installed before others are deleted, so switching
//! profiles never leaves a moment without the new profile's denies; an old
//! allow that lingers meanwhile still loses to any deny (no precedence).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use snitchwatch_proto::protocol::{Operator, Rule};
use tracing::warn;

use crate::blocklists::Enforcement;
use crate::cache::rules::{RulesCache, SharedRulesCache};
use crate::daemon_commands::{CommandError, DaemonCommands, ProfileCommand, SendError};
use crate::profiles::materializer::made_by_bridge;
use crate::rule_name::is_reserved_profile_name;

/// How long one profile rule command waits for the daemon.
pub const PROFILE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

const RULES_UNKNOWN_REASON: &str = "Snitchwatch doesn't have the firewall service's rule list \
     yet (it isn't connected, or it has more rules than Snitchwatch reads); the rule is \
     installed once it does";
const NOT_CONNECTED_REASON: &str =
    "The firewall service isn't connected; the rule is installed once it connects";
/// Longest daemon refusal text shown, in characters.
const MAX_DAEMON_TEXT_CHARS: usize = 200;

/// Applies the active profile's rules.
// clippy 1.99's `double_must_use` fires on async_trait's generated
// `#[must_use]` methods (they return an already-must_use boxed future).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ProfileRuleSink: Send + Sync + 'static {
    /// Make the daemon hold `wanted` and none of the bridge's other profile
    /// rules. One outcome per wanted rule, in order.
    async fn apply(&self, wanted: &[Rule]) -> Vec<Enforcement>;

    /// Why this sink installs nothing at all, if it doesn't.
    fn not_applied_reason(&self) -> Option<&str> {
        None
    }
}

/// Installs nothing, and says why: a per-user bridge, no saved state, or
/// an in-process bridge.
pub struct NoopProfileRuleSink {
    reason: String,
}

impl NoopProfileRuleSink {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl ProfileRuleSink for NoopProfileRuleSink {
    async fn apply(&self, wanted: &[Rule]) -> Vec<Enforcement> {
        let reason = self.reason.clone();
        vec![Enforcement::NotEnforced { reason }; wanted.len()]
    }

    fn not_applied_reason(&self) -> Option<&str> {
        Some(&self.reason)
    }
}

/// Installs profile rules in the daemon; see the module doc.
pub struct DaemonProfileSink {
    commands: DaemonCommands,
    rules: SharedRulesCache,
    timeout: Duration,
    /// Profile-named rules Snitchwatch didn't make, already logged.
    warned: StdMutex<BTreeSet<String>>,
}

/// Why a command didn't get an `OK`.
enum Failure {
    /// The daemon or the bridge refused it; plain text.
    Refused(String),
    /// The daemon couldn't be reached or didn't answer; plain text.
    Unavailable(String),
}

impl DaemonProfileSink {
    pub fn new(commands: DaemonCommands, rules: SharedRulesCache) -> Self {
        Self {
            commands,
            rules,
            timeout: PROFILE_COMMAND_TIMEOUT,
            warned: StdMutex::default(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn cache(&self) -> MutexGuard<'_, RulesCache> {
        self.rules
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Cached rules under the profile prefix, or `None` while Unknown.
    fn cached_profile_rules(&self) -> Option<HashMap<String, Rule>> {
        self.cache().rules().map(|rules| {
            rules
                .iter()
                .filter(|(name, _)| is_reserved_profile_name(name))
                .map(|(name, rule)| (name.clone(), rule.clone()))
                .collect()
        })
    }

    /// [`made_by_bridge`], warning once per name about one it didn't make.
    fn ours(&self, rule: &Rule) -> bool {
        let made = made_by_bridge(rule);
        let first = !made
            && self
                .warned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(rule.name.clone());
        if first {
            warn!(
                name_len = rule.name.len(),
                "leaving a profile-named rule Snitchwatch didn't make"
            );
        }
        made
    }

    async fn send(&self, command: ProfileCommand) -> Result<(), Failure> {
        let pending = self.commands.send_profile(command).map_err(send_failure)?;
        pending.wait(self.timeout).await.map_err(command_failure)
    }

    /// Delete the bridge's cached profile rules not in `wanted`. Stops at
    /// the first the daemon can't be reached for; a refusal is logged.
    async fn purge(&self, cached: &HashMap<String, Rule>, wanted: &[Rule]) {
        let keep: BTreeSet<&str> = wanted.iter().map(|r| r.name.as_str()).collect();
        let mut stale: Vec<&Rule> = cached
            .values()
            .filter(|rule| !keep.contains(rule.name.as_str()))
            .filter(|rule| self.ours(rule))
            .collect();
        stale.sort_by(|a, b| a.name.cmp(&b.name));
        for rule in stale {
            let Some(command) = ProfileCommand::delete(&rule.name) else {
                continue;
            };
            match self.send(command).await {
                Ok(()) => {}
                Err(Failure::Unavailable(reason)) => {
                    warn!(%reason, "stopped deleting old profile rules");
                    return;
                }
                Err(Failure::Refused(reason)) => {
                    warn!(%reason, "the daemon refused to delete an old profile rule")
                }
            }
        }
    }
}

#[async_trait]
impl ProfileRuleSink for DaemonProfileSink {
    async fn apply(&self, wanted: &[Rule]) -> Vec<Enforcement> {
        let Some(cached) = self.cached_profile_rules() else {
            let reason = RULES_UNKNOWN_REASON.to_string();
            return vec![Enforcement::Unconfirmed { reason }; wanted.len()];
        };
        let mut outcomes = Vec::with_capacity(wanted.len());
        let mut unreachable: Option<String> = None;
        for rule in wanted {
            if let Some(reason) = &unreachable {
                outcomes.push(Enforcement::Unconfirmed {
                    reason: reason.clone(),
                });
                continue;
            }
            if cached.get(&rule.name).is_some_and(|c| in_place(c, rule)) {
                outcomes.push(Enforcement::RuleInstalled { at: Utc::now() });
                continue;
            }
            outcomes.push(
                match self.send(ProfileCommand::install(rule.clone())).await {
                    Ok(()) => Enforcement::RuleInstalled { at: Utc::now() },
                    Err(Failure::Refused(reason)) => Enforcement::NotEnforced { reason },
                    Err(Failure::Unavailable(reason)) => {
                        unreachable = Some(reason.clone());
                        Enforcement::Unconfirmed { reason }
                    }
                },
            );
        }
        if unreachable.is_none() {
            self.purge(&cached, wanted).await;
        }
        outcomes
    }
}

/// Whether the daemon's copy `cached` is the rule `wanted`, as the daemon
/// echoes it: `rule_io::same_rule` (a list operand spelled either way,
/// `created` ignored), with a non-case-sensitive pattern compared the way
/// the daemon stores it, lowercased (`operator.go` `Compile`).
fn in_place(cached: &Rule, wanted: &Rule) -> bool {
    let fold = |rule: &Rule| Rule {
        operator: rule.operator.as_ref().map(fold_patterns),
        ..rule.clone()
    };
    crate::rule_io::same_rule(Some(&fold(cached)), Some(&fold(wanted)))
}

fn fold_patterns(op: &Operator) -> Operator {
    let mut op = op.clone();
    if op.r#type == "regexp" && !op.sensitive {
        op.data = op.data.to_lowercase();
    }
    op.list = op.list.iter().map(fold_patterns).collect();
    op
}

fn send_failure(error: SendError) -> Failure {
    match error {
        SendError::NoDaemon => Failure::Unavailable(NOT_CONNECTED_REASON.into()),
        SendError::NotQueued => {
            Failure::Unavailable("The firewall service is busy and didn't take the rule".into())
        }
        other => Failure::Refused(format!("Snitchwatch refused to send the rule ({other})")),
    }
}

fn command_failure(error: CommandError) -> Failure {
    match error {
        CommandError::Rejected(text) => {
            let text: String = crate::translator::verdict::strip_display_hazards(&text)
                .trim()
                .chars()
                .take(MAX_DAEMON_TEXT_CHARS)
                .collect();
            Failure::Refused(if text.is_empty() {
                "The firewall service refused the rule".to_string()
            } else {
                format!("The firewall service refused the rule: {text}")
            })
        }
        CommandError::Timeout => Failure::Unavailable("The firewall service didn't answer".into()),
        CommandError::StreamClosed => Failure::Unavailable(
            "The connection to the firewall service closed before it answered".into(),
        ),
    }
}

#[cfg(test)]
#[path = "enforcer_tests.rs"]
mod tests;
