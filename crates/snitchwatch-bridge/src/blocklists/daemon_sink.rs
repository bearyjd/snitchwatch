//! [`DaemonRuleSink`]: each subscription becomes opensnitchd `lists.*` deny
//! rules over files the bridge writes (issue #45 PR B).
//!
//! Install, for one list:
//! 1. write its list files ([`ListDir::write_list`], which leaves an
//!    unchanged file alone); on failure send nothing;
//! 2. `CHANGE_RULE` each kind's rule through
//!    [`DaemonCommands::send_blocklist`] and wait for the reply. Only an
//!    `OK` counts, unless the rule is already in place: identical in the
//!    daemon's committed rule snapshot (or, while there is none, confirmed
//!    earlier in this run) with its list file present. The daemon re-reads a
//!    changed file by itself every 4 s, so a refresh resends nothing, and a
//!    bridge restart doesn't resend every rule;
//! 3. delete the list's other rules Snitchwatch made (a kind it no longer
//!    has, legacy per-host rules), then the directories of kinds it no
//!    longer has.
//!
//! Remove deletes the rules first, then the list's directory. Nothing is
//! sent while the daemon's rule list is unknown (cache `Unknown`): the
//! reconcile after its next snapshot does it. Only rules Snitchwatch made
//! ([`made_by_bridge`]) are ever deleted on the strength of the cache.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use snitchwatch_proto::protocol::Rule;
use tracing::{error, warn};

use crate::blocklists::list_dir::{classify, IdComponent, ListDir, ListEntries};
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

/// The rules cache is `Unknown`: no daemon connected, or its rule snapshot
/// was over the size Snitchwatch reads (`MAX_SNAPSHOT_RULES`).
const RULES_UNKNOWN_REASON: &str = "Snitchwatch doesn't have the firewall service's rule list \
     yet (it isn't connected, or it has more rules than Snitchwatch reads); blocking starts \
     once it does";
const NOT_CONNECTED_REASON: &str =
    "The firewall service isn't connected; blocking starts once it connects";
/// How long an unsubscribed list's files are kept before an orphan purge
/// removes them: longer than opensnitchd's 30 s per-path reload limit, so a
/// quick resubscribe finds its files in place (issue #73).
pub const RELEASE_GRACE: Duration = Duration::from_secs(5 * 60);
/// Longest daemon refusal text shown to the user, in characters.
const MAX_DAEMON_TEXT_CHARS: usize = 200;

pub struct DaemonRuleSink {
    dir: Arc<ListDir>,
    commands: DaemonCommands,
    rules: SharedRulesCache,
    timeout: Duration,
    /// Rules the daemon answered `OK` to in this run, by name, as sent.
    confirmed: StdMutex<HashMap<String, Rule>>,
    /// Lists whose files were written or checked against their hosts in
    /// this run, with the kinds they hold.
    verified: StdMutex<HashMap<IdComponent, Vec<ListKind>>>,
    /// Blocklist-named rules Snitchwatch didn't make, already logged.
    warned: StdMutex<BTreeSet<String>>,
    /// Lists unsubscribed in this run whose rules are gone but whose files are
    /// kept, and since when ([`RELEASE_GRACE`]).
    released: StdMutex<HashMap<IdComponent, Instant>>,
    release_grace: Duration,
}

impl DaemonRuleSink {
    /// Also pins `dir`'s root as the only one `commands` sends blocklist
    /// rules for.
    pub fn new(dir: ListDir, commands: DaemonCommands, rules: SharedRulesCache) -> Self {
        if let Err(pinned) = commands.pin_blocklist_root(dir.root()) {
            error!(
                pinned = %pinned.display(),
                "another blocklist root is pinned; this sink's rules will be refused"
            );
        }
        Self {
            dir: Arc::new(dir),
            commands,
            rules,
            timeout: BLOCKLIST_COMMAND_TIMEOUT,
            confirmed: StdMutex::default(),
            verified: StdMutex::default(),
            warned: StdMutex::default(),
            released: StdMutex::default(),
            release_grace: RELEASE_GRACE,
        }
    }

    /// How long released lists keep their files. Tests.
    pub fn with_release_grace(mut self, grace: Duration) -> Self {
        self.release_grace = grace;
        self
    }

    fn released(&self) -> MutexGuard<'_, HashMap<IdComponent, Instant>> {
        self.released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn verified(&self) -> MutexGuard<'_, HashMap<IdComponent, Vec<ListKind>>> {
        self.verified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// [`made_by_bridge`], warning once per name and run about a rule under
    /// a blocklist name that Snitchwatch didn't make.
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
                "leaving a blocklist-named rule Snitchwatch didn't make"
            );
        }
        made
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

    /// Cached rules under a blocklist name, or `None` while Unknown.
    fn cached_blocklist_rules(&self) -> Option<Vec<Rule>> {
        self.cache().rules().map(|rules| {
            rules
                .values()
                .filter(|rule| is_reserved_blocklist_name(&rule.name))
                .cloned()
                .collect()
        })
    }

    /// Names of cached rules of `list` (either band) that Snitchwatch made.
    fn cached_rules_of(&self, list: &IdComponent) -> Vec<String> {
        let prefixes = owned_rule_name_prefixes(list);
        self.cached_blocklist_rules()
            .unwrap_or_default()
            .into_iter()
            .filter(|rule| prefixes.iter().any(|p| rule.name.starts_with(p.as_str())))
            .filter(|rule| self.ours(rule))
            .map(|rule| rule.name)
            .collect()
    }

    /// The kind's rule is in place and its list file is there: identical in
    /// the daemon's committed snapshot, or, while the cache is Unknown,
    /// confirmed in this run.
    fn in_place(&self, list: &IdComponent, kind: ListKind, rule: &Rule) -> bool {
        if !self.dir.has_list(list, kind) {
            return false;
        }
        match self.cache().rules() {
            Some(rules) => rules
                .get(&rule.name)
                .is_some_and(|cached| same_rule(cached, rule)),
            None => self
                .confirmed()
                .get(&rule.name)
                .is_some_and(|sent| same_rule(sent, rule)),
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
        if self.in_place(list, kind, &rule) {
            // Found in the snapshot counts as confirmed, for when the
            // daemon's list is next Unknown.
            self.confirmed().insert(rule.name.clone(), rule);
            return Ok(());
        }
        if !self.daemon_rules_known() {
            return Err(NotInstalled::daemon_unavailable(RULES_UNKNOWN_REASON));
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

    /// Write the non-empty kinds' files; returns those kinds.
    /// What to delete for `list`: with the daemon's rule list known, what it
    /// holds of ours; without it, our own two names (a delete of a missing
    /// rule is a no-op).
    fn rule_names_to_delete(&self, list: &IdComponent) -> BTreeSet<String> {
        if self.daemon_rules_known() {
            self.cached_rules_of(list).into_iter().collect()
        } else {
            ListKind::ALL
                .into_iter()
                .map(|kind| list_rule_name(list, kind))
                .collect()
        }
    }

    async fn write_files(
        &self,
        list: &IdComponent,
        entries: ListEntries,
    ) -> Result<Vec<ListKind>, NotInstalled> {
        let kinds: Vec<ListKind> = ListKind::ALL
            .into_iter()
            .filter(|kind| !entries.get(*kind).is_empty())
            .collect();
        let (writer_list, written) = (list.clone(), kinds.clone());
        self.verified().remove(list);
        self.files(move |dir| {
            written.iter().try_for_each(|kind| {
                dir.write_list(&writer_list, *kind, entries.get(*kind))
                    .map(drop)
            })
        })
        .await
        .map_err(|e| {
            NotInstalled::new(format!(
                "Couldn't save the list for the firewall service: {e}"
            ))
        })?;
        self.verified().insert(list.clone(), kinds.clone());
        Ok(kinds)
    }

    /// Delete the list's rules Snitchwatch made that aren't for `kinds`,
    /// then the directories of the other kinds.
    async fn remove_other_kinds(
        &self,
        list: &IdComponent,
        kinds: &[ListKind],
    ) -> Result<(), NotInstalled> {
        let wanted: BTreeSet<String> = kinds.iter().map(|k| list_rule_name(list, *k)).collect();
        let stale = self
            .cached_rules_of(list)
            .into_iter()
            .filter(|name| !wanted.contains(name));
        self.delete(stale).await?;
        let gone: Vec<ListKind> = ListKind::ALL
            .into_iter()
            .filter(|kind| !kinds.contains(kind))
            .collect();
        let list = list.clone();
        self.files(move |dir| {
            gone.iter()
                .try_for_each(|kind| dir.remove_kind(&list, *kind))
        })
        .await
        .map_err(|e| NotInstalled::new(format!("Couldn't remove an old list file: {e}")))
    }
}

#[async_trait]
impl RuleSink for DaemonRuleSink {
    fn daemon_rules_known(&self) -> bool {
        !self.cache().is_unknown()
    }

    fn is_current(&self, list_id: &str) -> bool {
        let list = IdComponent::from_id(list_id);
        let kinds: Vec<ListKind> = ListKind::ALL
            .into_iter()
            .filter(|kind| self.dir.has_list(&list, *kind))
            .collect();
        !kinds.is_empty()
            && self.daemon_rules_known()
            && kinds.iter().all(|kind| {
                BlocklistCommand::install(&list, *kind, &self.dir)
                    .rule()
                    .is_some_and(|rule| self.in_place(&list, *kind, rule))
            })
    }

    fn files_verified(&self, list_id: &str) -> bool {
        let list = IdComponent::from_id(list_id);
        let Some(kinds) = self.verified().get(&list).cloned() else {
            return false;
        };
        !kinds.is_empty() && kinds.iter().all(|kind| self.dir.has_list(&list, *kind))
    }

    async fn reinstall_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        let list = IdComponent::from_id(list_id);
        let kinds = self.verified().get(&list).cloned().unwrap_or_default();
        if kinds.is_empty() {
            return Err(NotInstalled::new(NO_HOSTS_REASON));
        }
        for kind in &kinds {
            self.install(&list, *kind).await?;
        }
        // The install that wrote these files may have been refused before it
        // got to the kinds the list no longer has; finish that now, or a stale
        // `ips.list` keeps blocking hosts the list dropped (issue #73).
        self.remove_other_kinds(&list, &kinds).await
    }

    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        let list = IdComponent::from_id(list_id);
        // Subscribed (again): its files are wanted.
        self.released().remove(&list);
        let kinds = self.write_files(&list, classify(hosts)).await?;
        for kind in &kinds {
            self.install(&list, *kind).await?;
        }
        self.remove_other_kinds(&list, &kinds).await?;
        if kinds.is_empty() {
            return Err(NotInstalled::new(NO_HOSTS_REASON));
        }
        Ok(())
    }

    async fn release_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        let list = IdComponent::from_id(list_id);
        let names = self.rule_names_to_delete(&list);
        let deleted = self.delete(names).await;
        self.verified().remove(&list);
        if deleted.is_err() {
            // A rule the daemon couldn't be told to delete must not go on
            // blocking a list the user dropped: without its files it reads
            // nothing, and the next reconcile deletes it.
            return self.remove_blocklist_rules(list_id).await.and(deleted);
        }
        self.released().insert(list, Instant::now());
        Ok(())
    }

    async fn remove_blocklist_rules(&self, list_id: &str) -> Result<(), NotInstalled> {
        let list = IdComponent::from_id(list_id);
        let names = self.rule_names_to_delete(&list);
        let deleted = self.delete(names).await;
        self.verified().remove(&list);
        // Even when the daemon is gone: a rule left behind then reads an
        // empty directory, and the next reconcile deletes it.
        self.files(move |dir| dir.remove_list(&list))
            .await
            .map_err(|e| NotInstalled::new(format!("Couldn't remove the list's files: {e}")))?;
        deleted
    }

    async fn remove_orphans(&self, keep: &[String]) {
        let keep: BTreeSet<IdComponent> = keep.iter().map(|id| IdComponent::from_id(id)).collect();
        let Some(cached) = self.cached_blocklist_rules() else {
            return;
        };
        let orphans: Vec<String> = cached
            .into_iter()
            .filter(|rule| {
                let kept = list_of_rule_name(&rule.name)
                    .and_then(IdComponent::parse)
                    .filter(|list| keep.contains(list));
                match kept {
                    Some(list) => !ListKind::ALL
                        .into_iter()
                        .any(|kind| list_rule_name(&list, kind) == rule.name),
                    None => true,
                }
            })
            .filter(|rule| self.ours(rule))
            .map(|rule| rule.name)
            .collect();
        if let Err(e) = self.delete(orphans).await {
            warn!(reason = %e.reason, "stopped deleting orphaned blocklist rules");
            return;
        }
        self.verified().retain(|list, _| keep.contains(list));
        // Directories of lists released a moment ago stay (see `RELEASE_GRACE`);
        // any other directory nobody subscribes to goes.
        let now = Instant::now();
        let grace = self.release_grace;
        let mut kept_for_now = BTreeSet::new();
        {
            let mut released = self.released();
            released.retain(|list, _| !keep.contains(list));
            released.retain(|list, at| {
                let waiting = now.saturating_duration_since(*at) < grace;
                if waiting {
                    kept_for_now.insert(list.clone());
                }
                waiting
            });
        }
        let removed = self
            .files(move |dir| {
                for list in dir.lists()? {
                    if !keep.contains(&list) && !kept_for_now.contains(&list) {
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

/// Whether Snitchwatch made a rule under a blocklist name: its `lists` deny
/// shape, or the blocklist tag in its description (legacy per-host rules).
/// Anything else under the prefix was made by someone else and is never
/// deleted on the strength of its name.
pub fn made_by_bridge(rule: &Rule) -> bool {
    let tagged = serde_json::from_str::<serde_json::Value>(&rule.description)
        .is_ok_and(|v| v["snitchwatch"]["source"] == "blocklist");
    let shaped = rule.action == "deny"
        && rule.operator.as_ref().is_some_and(|op| {
            op.r#type == "lists" && ListKind::from_operand(&op.operand).is_some()
        });
    tagged || shaped
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
pub(in crate::blocklists) mod tests;

#[cfg(test)]
#[path = "daemon_sink_followup_tests.rs"]
mod followup_tests;
