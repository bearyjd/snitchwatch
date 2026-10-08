//! [`BlocklistsManager`]'s install decisions (issue #45 PR B): the total
//! size limit, and the reconcile that brings the daemon in line with the
//! subscriptions.

use tracing::{error, warn};

use super::BlocklistsManager;
use crate::blocklists::store::Subscription;
use crate::blocklists::{
    thousands, BlocklistEvent, Enforcement, NotInstalled, ReconcileScope, OVER_LIMIT_REASON_PREFIX,
    STORE_ERROR_REASON,
};

impl BlocklistsManager {
    /// Every subscription in the order it was subscribed.
    pub(super) fn subscriptions_in_order(&self) -> Vec<Subscription> {
        let order = self.order().clone();
        let cache = self.cache();
        let mut subs: Vec<Subscription> = order
            .iter()
            .filter_map(|id| cache.get(id).cloned())
            .collect();
        subs.extend(cache.values().filter(|s| !order.contains(&s.id)).cloned());
        subs
    }

    /// `Some(reason)` when `id` and the lists subscribed before it hold more
    /// than [`AGGREGATE_MAX_HOSTS`] hosts.
    pub(super) fn over_aggregate_cap(&self, id: &str) -> Option<String> {
        let mut total: u64 = 0;
        for sub in self.subscriptions_in_order() {
            total = total.saturating_add(u64::try_from(sub.entry_count).unwrap_or(0));
            if sub.id == id {
                return (total > self.aggregate_cap).then(|| {
                    format!(
                        "{OVER_LIMIT_REASON_PREFIX}: this list and the ones subscribed before it \
                         hold {} hosts; Snitchwatch applies at most {}. Remove a list to make \
                         room.",
                        thousands(total),
                        thousands(self.aggregate_cap)
                    )
                });
            }
        }
        None
    }

    /// Lists other than `except` that the lists' current sizes put past the
    /// total size limit are taken off the daemon now and told why, unless they
    /// already were. `except` (a list just refreshed) is installed or demoted
    /// by the caller, after this.
    pub(super) async fn demote_lists_past_the_limit(&self, except: &str) {
        for sub in self.subscriptions_in_order() {
            if sub.id == except {
                continue;
            }
            let Some(reason) = self.over_aggregate_cap(&sub.id) else {
                continue;
            };
            let already = matches!(self.enforcement(&sub.id),
                Enforcement::NotEnforced { reason } if reason.starts_with(OVER_LIMIT_REASON_PREFIX));
            if already {
                continue;
            }
            let outcome = self.remove_over_limit(&sub.id, reason).await;
            self.record_install(&sub.id, &outcome);
            let _ = self.bus.send(BlocklistEvent::StatusChanged {
                subscription_id: sub.id.clone(),
            });
        }
    }

    /// Past the total size limit, remove what `id` had and say why.
    async fn remove_over_limit(&self, id: &str, reason: String) -> Result<(), NotInstalled> {
        let removed = self.rule_sink.remove_blocklist_rules(id).await;
        if let Err(e) = &removed {
            warn!(%id, reason = %e.reason, "couldn't remove an over-limit list's rules");
        }
        Err(NotInstalled {
            reason,
            daemon_unavailable: removed.is_err_and(|e| e.daemon_unavailable),
        })
    }

    /// Install `hosts` for `id`, or, past the total size limit, remove what
    /// it had and say why.
    pub(super) async fn install(&self, id: &str, hosts: Vec<String>) -> Result<(), NotInstalled> {
        match self.over_aggregate_cap(id) {
            Some(reason) => {
                drop(hosts);
                self.remove_over_limit(id, reason).await
            }
            None => {
                let outcome = self.rule_sink.replace_blocklist_rules(id, hosts).await;
                self.track_install(id, outcome)
            }
        }
    }

    /// [`reconcile_with`](Self::reconcile_with) a full pass.
    pub async fn reconcile(&self) {
        self.reconcile_with(ReconcileScope::Full).await;
    }

    /// Bring the daemon in line with the subscriptions (issue #45 PR B), in
    /// subscription order:
    /// - a list past [`AGGREGATE_MAX_HOSTS`] has its rules removed, before
    ///   any of its rows are read;
    /// - a list in place whose files were checked in this run is reported
    ///   installed;
    /// - a [`ReconcileScope::CleanUp`] pass skips lists already tried;
    /// - a list whose files were checked in this run has only its rules
    ///   resent; any other downloaded list is installed from the store (the
    ///   first full pass of a run so checks, and repairs, every list file).
    ///
    /// Then rules and files of lists no longer subscribed are deleted. Does
    /// nothing while the stored subscriptions are unreadable or the daemon's
    /// rule list is unknown, and stops at the first list the daemon can't be
    /// reached for.
    pub async fn reconcile_with(&self, scope: ReconcileScope) {
        self.reconcile_skipping(scope, &[], false).await;
    }

    /// A full pass after a refresh tick: lists in `tried` were installed (or
    /// refused) by the tick's downloads and aren't tried again by it, and a
    /// list the daemon refused is left alone until its backoff
    /// ([`REFUSAL_BACKOFF_MINUTES`](crate::blocklists::REFUSAL_BACKOFF_MINUTES))
    /// is over.
    pub async fn reconcile_after_refresh(&self, tried: &[String]) {
        self.reconcile_skipping(ReconcileScope::Full, tried, true)
            .await;
    }

    async fn reconcile_skipping(&self, scope: ReconcileScope, skip: &[String], backoff: bool) {
        self.announce_leftover_change();
        if !self.installs_rules() || !self.rule_sink.daemon_rules_known() {
            return;
        }
        for sub in self.subscriptions_in_order() {
            if sub.last_fetched_at.is_none() || skip.contains(&sub.id) {
                continue;
            }
            let before = self.enforcement(&sub.id);
            let Some(outcome) = self.reconcile_one(&sub, scope, &before, backoff).await else {
                continue;
            };
            let stop = matches!(&outcome, Err(e) if e.daemon_unavailable);
            self.record_install(&sub.id, &outcome);
            if self.enforcement(&sub.id) != before {
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: sub.id.clone(),
                });
            }
            if stop {
                return;
            }
        }
        if !self.rule_sink.daemon_rules_known() {
            return;
        }
        let keep: Vec<String> = self.cache().keys().cloned().collect();
        self.rule_sink.remove_orphans(&keep).await;
    }

    /// One list's outcome, or `None` when there is nothing to record.
    async fn reconcile_one(
        &self,
        sub: &Subscription,
        scope: ReconcileScope,
        before: &Enforcement,
        backoff: bool,
    ) -> Option<Result<(), NotInstalled>> {
        let id = sub.id.as_str();
        if let Some(reason) = self.over_aggregate_cap(id) {
            return Some(self.remove_over_limit(id, reason).await);
        }
        let checked = self.rule_sink.files_verified(id);
        if checked && self.rule_sink.is_current(id) {
            return (!matches!(before, Enforcement::RuleInstalled { .. })).then_some(Ok(()));
        }
        let tried = matches!(
            before,
            Enforcement::NotEnforced { .. } | Enforcement::RuleInstalled { .. }
        );
        if scope == ReconcileScope::CleanUp && tried {
            return None;
        }
        if backoff && matches!(before, Enforcement::NotEnforced { .. }) && self.backing_off(id) {
            return None;
        }
        if checked {
            let outcome = self.rule_sink.reinstall_blocklist_rules(id).await;
            return Some(self.track_install(id, outcome));
        }
        let owned = id.to_string();
        Some(
            match self.with_store(move |s| s.list_entries(&owned)).await {
                Ok(hosts) => self.install(id, hosts).await,
                Err(e) => {
                    error!(%id, error = %e, "couldn't read a stored blocklist");
                    Err(NotInstalled::new(STORE_ERROR_REASON))
                }
            },
        )
    }
}
