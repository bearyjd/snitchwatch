//! [`DaemonRuleSink`]: each subscription becomes opensnitchd `lists.*` deny
//! rules over files the bridge writes (issue #45 PR B).
//!
//! Install, for one list:
//! 1. write its list files ([`ListDir::write_list`]); on failure send
//!    nothing;
//! 2. `CHANGE_RULE` each kind's rule through
//!    [`DaemonCommands::send_blocklist`] and wait for the reply. Only an
//!    `OK` for every rule counts; a rule confirmed earlier in this run and
//!    still identical in the daemon's list isn't resent (the daemon re-reads
//!    a changed file by itself every 4 s);
//! 3. delete the list's other rules (a kind it no longer has, legacy
//!    per-host rules), then the directories of kinds it no longer has.
//!
//! Remove deletes the rules first, then the list's directory. Nothing is
//! sent while the daemon's rule list is unknown (cache `Unknown`): the
//! reconcile after its next snapshot does it.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use snitchwatch_proto::protocol::Rule;
use tracing::warn;

use crate::blocklists::list_dir::{classify, IdComponent, ListDir};
use crate::blocklists::materializer::{
    list_of_rule_name, list_rule_name, owned_rule_name_prefixes, ListKind,
};
use crate::blocklists::{NotInstalled, RuleSink, NO_HOSTS_REASON};
use crate::cache::rules::{RulesCache, SharedRulesCache};
use crate::daemon_commands::{BlocklistCommand, CommandError, DaemonCommands, SendError};
use crate::rule_name::is_reserved_blocklist_name;

/// How long a blocklist rule command waits for the daemon. Longer than a GUI
/// toggle's: replacing a `lists` rule stops the old rule's list monitor,
/// which only notices between its 4 s polls (`operator_lists.go`).
pub const BLOCKLIST_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

const NOT_CONNECTED_REASON: &str =
    "The firewall service isn't connected; blocking starts once it connects";
/// Longest daemon refusal text shown to the user, in characters.
const MAX_DAEMON_TEXT_CHARS: usize = 200;

pub struct DaemonRuleSink {
    dir: Arc<ListDir>,
    commands: DaemonCommands,
    rules: SharedRulesCache,
    timeout: Duration,
    /// Rules the daemon answered `OK` to in this run, by name, as sent.
    confirmed: StdMutex<HashMap<String, Rule>>,
}

impl DaemonRuleSink {
    pub fn new(dir: ListDir, commands: DaemonCommands, rules: SharedRulesCache) -> Self {
        Self {
            dir: Arc::new(dir),
            commands,
            rules,
            timeout: BLOCKLIST_COMMAND_TIMEOUT,
            confirmed: StdMutex::default(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn confirmed(&self) -> MutexGuard<'_, HashMap<String, Rule>> {
        self.confirmed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cache(&self) -> MutexGuard<'_, RulesCache> {
        self.rules
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Cached rule names under a blocklist prefix, or `None` while Unknown.
    fn cached_blocklist_names(&self) -> Option<Vec<String>> {
        match &*self.cache() {
            RulesCache::Unknown => None,
            RulesCache::Synced(rules) => Some(
                rules
                    .keys()
                    .filter(|name| is_reserved_blocklist_name(name))
                    .cloned()
                    .collect(),
            ),
        }
    }

    /// Confirmed in this run and, when the daemon's list is known, still in
    /// it unchanged.
    fn in_place(&self, rule: &Rule) -> bool {
        let confirmed = self
            .confirmed()
            .get(&rule.name)
            .is_some_and(|sent| same_rule(sent, rule));
        confirmed
            && match &*self.cache() {
                RulesCache::Unknown => true,
                RulesCache::Synced(rules) => rules
                    .get(&rule.name)
                    .is_some_and(|cached| same_rule(cached, rule)),
            }
    }

    async fn send(&self, command: BlocklistCommand) -> Result<(), NotInstalled> {
        let pending = self
            .commands
            .send_blocklist(command)
            .map_err(send_failure)?;
        pending.wait(self.timeout).await.map_err(command_failure)
    }

    async fn install(&self, list: &IdComponent, kind: ListKind) -> Result<(), NotInstalled> {
        let command = BlocklistCommand::install(list, kind, &self.dir);
        let Some(rule) = command.rule().cloned() else {
            return Err(NotInstalled::new("Snitchwatch built no rule for this list"));
        };
        if self.in_place(&rule) {
            return Ok(());
        }
        if self.cached_blocklist_names().is_none() {
            return Err(NotInstalled::daemon_unavailable(NOT_CONNECTED_REASON));
        }
        self.send(command).await?;
        self.confirmed().insert(rule.name.clone(), rule);
        Ok(())
    }

    /// Delete `names`, stopping at the first the daemon can't be reached
    /// for. A refused delete is logged and skipped.
    async fn delete(&self, names: impl IntoIterator<Item = String>) -> Result<(), NotInstalled> {
        for name in names {
            let Some(command) = BlocklistCommand::delete(&name) else {
                continue;
            };
            match self.send(command).await {
                Ok(()) => {
                    self.confirmed().remove(&name);
                }
                Err(e) if e.daemon_unavailable => return Err(e),
                Err(e) => warn!(reason = %e.reason, "daemon refused to delete a blocklist rule"),
            }
        }
        Ok(())
    }

    /// Blocking file work on the blocking pool.
    async fn files<T: Send + 'static>(
        &self,
        work: impl FnOnce(&ListDir) -> std::io::Result<T> + Send + 'static,
    ) -> std::io::Result<T> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || work(&dir))
            .await
            .map_err(std::io::Error::other)?
    }
}

#[async_trait]
impl RuleSink for DaemonRuleSink {
    fn daemon_rules_known(&self) -> bool {
        self.cached_blocklist_names().is_some()
    }

    fn is_current(&self, list_id: &str) -> bool {
        let list = IdComponent::from_id(list_id);
        let installed: Vec<Rule> = ListKind::ALL
            .into_iter()
            .filter(|kind| self.dir.has_list(&list, *kind))
            .filter_map(|kind| {
                BlocklistCommand::install(&list, kind, &self.dir)
                    .rule()
                    .cloned()
            })
            .collect();
        !installed.is_empty()
            && self.daemon_rules_known()
            && installed.iter().all(|rule| self.in_place(rule))
    }

    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        let list = IdComponent::from_id(list_id);
        let entries = classify(hosts);
        let kinds: Vec<ListKind> = ListKind::ALL
            .into_iter()
            .filter(|kind| !entries.get(*kind).is_empty())
            .collect();
        let (writer_list, writer_kinds) = (list.clone(), kinds.clone());
        self.files(move |dir| {
            writer_kinds
                .iter()
                .try_for_each(|kind| dir.write_list(&writer_list, *kind, entries.get(*kind)))
        })
        .await
        .map_err(|e| {
            NotInstalled::new(format!(
                "Couldn't save the list for the firewall service: {e}"
            ))
        })?;

        for kind in &kinds {
            self.install(&list, *kind).await?;
        }
        let wanted: BTreeSet<String> = kinds.iter().map(|k| list_rule_name(&list, *k)).collect();
        let prefixes = owned_rule_name_prefixes(&list);
        let stale = self
            .cached_blocklist_names()
            .unwrap_or_default()
            .into_iter()
            .filter(|name| prefixes.iter().any(|p| name.starts_with(p.as_str())))
            .filter(|name| !wanted.contains(name));
        self.delete(stale).await?;
        let gone: Vec<ListKind> = ListKind::ALL
            .into_iter()
            .filter(|kind| !kinds.contains(kind))
            .collect();
        let remover_list = list.clone();
        self.files(move |dir| {
            gone.iter()
                .try_for_each(|kind| dir.remove_kind(&remover_list, *kind))
        })
        .await
        .map_err(|e| NotInstalled::new(format!("Couldn't remove an old list file: {e}")))?;
        if kinds.is_empty() {
            return Err(NotInstalled::new(NO_HOSTS_REASON));
        }
        Ok(())
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        let list = IdComponent::from_id(list_id);
        let prefixes = owned_rule_name_prefixes(&list);
        let mut names: BTreeSet<String> = ListKind::ALL
            .into_iter()
            .map(|kind| list_rule_name(&list, kind))
            .collect();
        names.extend(
            self.cached_blocklist_names()
                .unwrap_or_default()
                .into_iter()
                .filter(|name| prefixes.iter().any(|p| name.starts_with(p.as_str()))),
        );
        let deleted = self.delete(names).await;
        // Even when the daemon is gone: a rule left behind then reads an
        // empty directory, and the next reconcile deletes it.
        self.files(move |dir| dir.remove_list(&list))
            .await
            .map_err(|e| NotInstalled::new(format!("Couldn't remove the list's files: {e}")))?;
        deleted
    }

    async fn remove_orphans(&self, keep: &[String]) {
        let keep: BTreeSet<IdComponent> = keep.iter().map(|id| IdComponent::from_id(id)).collect();
        let Some(cached) = self.cached_blocklist_names() else {
            return;
        };
        let orphans = cached.into_iter().filter(|name| {
            let kept = list_of_rule_name(name)
                .and_then(IdComponent::parse)
                .filter(|list| keep.contains(list));
            match kept {
                Some(list) => !ListKind::ALL
                    .into_iter()
                    .any(|kind| list_rule_name(&list, kind) == *name),
                None => true,
            }
        });
        if let Err(e) = self.delete(orphans).await {
            warn!(reason = %e.reason, "stopped deleting orphaned blocklist rules");
            return;
        }
        let removed = self
            .files(move |dir| {
                for list in dir.lists()? {
                    if !keep.contains(&list) {
                        dir.remove_list(&list)?;
                    }
                }
                Ok(())
            })
            .await;
        if let Err(e) = removed {
            warn!(error = %e, "couldn't remove orphaned blocklist files");
        }
    }
}

/// The fields that decide what a rule does; `created` and `description`
/// don't.
fn same_rule(a: &Rule, b: &Rule) -> bool {
    a.name == b.name
        && a.enabled == b.enabled
        && a.action == b.action
        && a.duration == b.duration
        && a.precedence == b.precedence
        && a.operator == b.operator
}

fn send_failure(error: SendError) -> NotInstalled {
    match error {
        SendError::NoDaemon => NotInstalled::daemon_unavailable(NOT_CONNECTED_REASON),
        SendError::NotQueued => NotInstalled::daemon_unavailable(
            "The firewall service is busy and didn't take the rule",
        ),
        other => NotInstalled::new(format!("Snitchwatch refused to send the rule ({other})")),
    }
}

fn command_failure(error: CommandError) -> NotInstalled {
    match error {
        CommandError::Rejected(text) => {
            let text: String = crate::translator::verdict::strip_display_hazards(&text)
                .trim()
                .chars()
                .take(MAX_DAEMON_TEXT_CHARS)
                .collect();
            NotInstalled::new(if text.is_empty() {
                "The firewall service refused the blocklist rule".to_string()
            } else {
                format!("The firewall service refused the blocklist rule: {text}")
            })
        }
        CommandError::Timeout => {
            NotInstalled::daemon_unavailable("The firewall service didn't answer")
        }
        CommandError::StreamClosed => NotInstalled::daemon_unavailable(
            "The connection to the firewall service closed before it answered",
        ),
    }
}

#[cfg(test)]
#[path = "daemon_sink_tests.rs"]
mod tests;
