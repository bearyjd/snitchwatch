//! Downloading a list and saving it (issues #45, #67, #73): what
//! [`BlocklistsManager::refresh_now`] does, which lists a tick finds due, and
//! the limit on the hosts kept on disk.

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use super::BlocklistsManager;
use crate::blocklists::fetcher::FetchOutcome;
use crate::blocklists::store::{FetchStatus, Subscription};
use crate::blocklists::{
    thousands, BlocklistEvent, Enforcement, NotInstalled, FAILED_RETRY_SECS, NOT_DOWNLOADED_REASON,
    OVER_LIMIT_REASON_PREFIX, STORAGE_LIMIT_REASON_PREFIX, STORE_ERROR_REASON,
};

/// What saving a download did.
enum Saved {
    /// Saved; the hosts come back for the sink.
    Hosts(Vec<String>),
    /// The subscription was removed meanwhile: nothing to do.
    Removed,
    /// The store failed; the list keeps its earlier hosts and shows this.
    Failed(FetchStatus),
}

impl BlocklistsManager {
    /// Download `id` now and update the store, memory and GUIs. A failed
    /// download keeps the cached entries. A download that would pass the
    /// limit on saved hosts counts as failed, and when the lists before `id`
    /// already fill that limit nothing is downloaded at all.
    pub async fn refresh_now(&self, id: &str) -> anyhow::Result<FetchStatus> {
        let Some(mut sub) = self.subscription(id) else {
            anyhow::bail!("unknown subscription: {id}");
        };
        self.waiting_for_room().remove(id);
        self.note_attempt(&mut sub).await?;
        let outcome = self.fetch_in_room(&sub).await;
        let now = Utc::now();
        match outcome {
            FetchOutcome::Ok { hosts, .. } => Ok(self.store_download(sub, hosts, now).await),
            FetchOutcome::Failed { reason } => {
                Ok(self.note_failed_download(sub, reason, now).await)
            }
        }
    }

    /// Saved before the download: a bridge killed mid-download or mid-parse
    /// backs off on restart instead of retrying at once.
    async fn note_attempt(&self, sub: &mut Subscription) -> anyhow::Result<()> {
        sub.last_attempt_at = Some(Utc::now());
        let row = sub.clone();
        match self.with_store(move |s| s.update_subscription(&row)).await {
            Ok(false) => anyhow::bail!("subscription removed: {}", sub.id),
            Ok(true) => {}
            Err(e) => error!(id = %sub.id, error = %e, "couldn't record a download attempt"),
        }
        self.cache().insert(sub.id.clone(), sub.clone());
        Ok(())
    }

    /// Download `sub`, unless there is no room to save any of it, and call a
    /// download larger than the room failed.
    async fn fetch_in_room(&self, sub: &Subscription) -> FetchOutcome {
        let room = self.stored_room(&sub.id);
        if room == 0 {
            return self.no_room();
        }
        match self.fetcher.fetch(&sub.url).await {
            FetchOutcome::Ok { hosts, .. } if hosts.len() as u64 > room => self.no_room(),
            outcome => outcome,
        }
    }

    fn no_room(&self) -> FetchOutcome {
        FetchOutcome::Failed {
            reason: format!(
                "{STORAGE_LIMIT_REASON_PREFIX} {} hosts across all lists",
                thousands(self.stored_cap)
            ),
        }
    }

    /// How many more hosts may be saved for `id` under the limit
    /// ([`STORED_MAX_HOSTS`](crate::blocklists::STORED_MAX_HOSTS)). Like the
    /// total size limit, the earliest lists get the budget: only the lists
    /// subscribed before `id` count against it (its own old copy is
    /// replaced), so a list that is over can't stop an earlier one from
    /// refreshing. A store that is already over the limit (from before it
    /// existed) leaves the later lists no room until others are removed.
    fn stored_room(&self, id: &str) -> u64 {
        let mut used: u64 = 0;
        for sub in self.subscriptions_in_order() {
            if sub.id == id {
                break;
            }
            used = used.saturating_add(u64::try_from(sub.entry_count).unwrap_or(0));
        }
        self.stored_cap.saturating_sub(used)
    }

    async fn note_failed_download(
        &self,
        sub: Subscription,
        reason: String,
        now: DateTime<Utc>,
    ) -> FetchStatus {
        let mut updated = sub;
        updated.last_attempt_at = Some(now);
        updated.last_fetch_status = FetchStatus::Failed {
            reason: reason.clone(),
        };
        let row = updated.clone();
        match self.with_store(move |s| s.update_subscription(&row)).await {
            Ok(false) => return updated.last_fetch_status,
            Ok(true) => {}
            Err(e) => {
                error!(id = %updated.id, error = %e, "couldn't record a failed download")
            }
        }
        if self.installs_rules() {
            self.enforcement_map()
                .entry(updated.id.clone())
                .or_insert_with(|| Enforcement::NotEnforced {
                    reason: NOT_DOWNLOADED_REASON.to_string(),
                });
        }
        self.cache().insert(updated.id.clone(), updated.clone());
        let _ = self.bus.send(BlocklistEvent::StatusChanged {
            subscription_id: updated.id.clone(),
        });
        warn!(id = %updated.id, %reason, "blocklist refresh failed; cache preserved");
        FetchStatus::Failed { reason }
    }

    async fn store_download(
        &self,
        sub: Subscription,
        hosts: Vec<String>,
        now: DateTime<Utc>,
    ) -> FetchStatus {
        let mut updated = sub;
        updated.entry_count = hosts.len() as i64;
        updated.last_fetched_at = Some(now);
        updated.last_attempt_at = Some(now);
        updated.last_fetch_status = FetchStatus::Ok;
        let count = hosts.len();
        let hosts = match self.save_hosts(&updated, hosts).await {
            Saved::Hosts(hosts) => hosts,
            Saved::Removed => return FetchStatus::Ok,
            Saved::Failed(status) => return status,
        };
        let id = updated.id.clone();
        self.cache().insert(id.clone(), updated);
        let daemon_down = self.clear_lists_past_the_saved_limit().await;
        if self.installs_rules() {
            self.install_downloaded(&id, hosts, daemon_down).await;
        }
        let _ = self.bus.send(BlocklistEvent::EntriesChanged {
            subscription_id: id.clone(),
        });
        let _ = self.bus.send(BlocklistEvent::StatusChanged {
            subscription_id: id.clone(),
        });
        info!(%id, count, "blocklist refreshed");
        FetchStatus::Ok
    }

    /// Replace `updated`'s saved hosts and row in one step.
    async fn save_hosts(&self, updated: &Subscription, hosts: Vec<String>) -> Saved {
        let row = updated.clone();
        // The hosts come back out of the blocking task for the sink, so up
        // to `MAX_ENTRIES` strings are never cloned.
        let saved = self
            .with_store(move |s| {
                s.replace_entries_and_update(&row, &hosts)
                    .map(|ok| (ok, hosts))
            })
            .await;
        match saved {
            Ok((true, hosts)) => Saved::Hosts(hosts),
            Ok((false, _)) => Saved::Removed,
            Err(e) => {
                error!(id = %updated.id, error = %e, "couldn't store a downloaded blocklist");
                let mut failed = updated.clone();
                failed.entry_count = self.subscription(&failed.id).map_or(0, |s| s.entry_count);
                failed.last_fetched_at = None;
                failed.last_fetch_status = FetchStatus::Failed {
                    reason: STORE_ERROR_REASON.to_string(),
                };
                let status = failed.last_fetch_status.clone();
                let id = failed.id.clone();
                self.cache().insert(id.clone(), failed);
                let _ = self.bus.send(BlocklistEvent::StatusChanged {
                    subscription_id: id,
                });
                Saved::Failed(status)
            }
        }
    }

    /// Put a saved download on the daemon. This list may have grown past what
    /// later lists left room for: those come off the daemon first, so it
    /// never holds more than the limit, even for a moment. A daemon that
    /// didn't answer them isn't asked again for this list.
    async fn install_downloaded(&self, id: &str, hosts: Vec<String>, daemon_down: bool) {
        let outcome = if daemon_down || self.demote_lists_past_the_limit(id).await {
            Err(NotInstalled::daemon_unavailable(
                "The firewall service didn't answer",
            ))
        } else {
            self.install(id, hosts).await
        };
        self.record_install(id, &outcome);
    }

    /// The hard bound on disk use: walk the lists in subscription order and
    /// clear the saved hosts of any whose running total, over the lists kept
    /// before it, passes the limit ([`STORED_MAX_HOSTS`](crate::blocklists::STORED_MAX_HOSTS)).
    /// A list's room is only measured against earlier lists, so a later one
    /// can come to hold hosts the earlier ones then outgrow; this undoes that
    /// after every save, so the total ends each refresh within the limit
    /// (during a save it can pass it by that list's size, at most
    /// `format::MAX_ENTRIES` hosts). A cleared list is already past the total
    /// size limit, which is the smaller, so it is off the daemon (this takes
    /// it off if not): only browsing its hosts is lost. Returns whether the
    /// daemon stopped answering meanwhile.
    pub(super) async fn clear_lists_past_the_saved_limit(&self) -> bool {
        let mut daemon_down = false;
        for id in lists_past_the_limit(&self.subscriptions_in_order(), self.stored_cap) {
            let Some(sub) = self.subscription(&id) else {
                continue;
            };
            let row = self.cleared_row(&sub);
            let saved = row.clone();
            match self
                .with_store(move |s| s.replace_entries_and_update(&saved, &[]))
                .await
            {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    error!(%id, error = %e, "couldn't clear a list past the saved-hosts limit");
                    continue;
                }
            }
            self.cache().insert(id.clone(), row);
            warn!(%id, "cleared a list's saved hosts: the lists before it fill the saved-hosts limit");
            if self.installs_rules() {
                let outcome = self
                    .remove_over_limit(&id, self.cleared_enforcement_reason(), daemon_down)
                    .await;
                daemon_down |= matches!(&outcome, Err(e) if e.daemon_unavailable);
                self.record_install(&id, &outcome);
            }
            let _ = self.bus.send(BlocklistEvent::EntriesChanged {
                subscription_id: id.clone(),
            });
            let _ = self.bus.send(BlocklistEvent::StatusChanged {
                subscription_id: id,
            });
        }
        daemon_down
    }

    /// The same walk when the bridge starts, over what an earlier run (or a
    /// version from before the limit) left: store and memory only. Such a
    /// list is past the enforcement limit too, so no rule of it is installed.
    pub(super) fn clear_lists_past_the_saved_limit_at_start(&self) {
        for id in lists_past_the_limit(&self.subscriptions_in_order(), self.stored_cap) {
            let Some(sub) = self.subscription(&id) else {
                continue;
            };
            let row = self.cleared_row(&sub);
            match self.store.replace_entries_and_update(&row, &[]) {
                Ok(true) => {
                    warn!(%id, "cleared a list's saved hosts at start: over the saved-hosts limit");
                    self.cache().insert(id, row);
                }
                Ok(false) => {}
                Err(e) => {
                    error!(%id, error = %e, "couldn't clear a list past the saved-hosts limit")
                }
            }
        }
    }

    /// `sub` with its saved hosts gone: no hosts, no download time (nothing
    /// is saved from one), and the reason.
    fn cleared_row(&self, sub: &Subscription) -> Subscription {
        Subscription {
            entry_count: 0,
            last_fetched_at: None,
            last_fetch_status: FetchStatus::Failed {
                reason: format!(
                    "{STORAGE_LIMIT_REASON_PREFIX} {} hosts across all lists, so this list's \
                     saved hosts were removed to keep the lists before it. It is past the total \
                     size limit, so it blocks nothing; only browsing its hosts is lost. Remove a \
                     list to make room.",
                    thousands(self.stored_cap)
                ),
            },
            ..sub.clone()
        }
    }

    /// What a cleared list's enforcement says; GUIs key their warning on the
    /// prefix.
    pub(super) fn cleared_enforcement_reason(&self) -> String {
        format!(
            "{OVER_LIMIT_REASON_PREFIX}: the lists before it fill the {} hosts Snitchwatch \
             saves, so its saved hosts were removed and it blocks nothing. Remove a list to \
             make room.",
            thousands(self.stored_cap)
        )
    }

    /// Ids whose refresh interval elapsed, that were never downloaded, whose
    /// failed download is past its retry backoff, or that were refused for
    /// room which a list since removed may have freed.
    pub fn due_subscription_ids(&self) -> Vec<String> {
        let now = Utc::now();
        let waiting = self.waiting_for_room().clone();
        self.cache()
            .values()
            .filter(|s| is_due(s, now) || waiting.contains(&s.id))
            .map(|s| s.id.clone())
            .collect()
    }

    /// A list was removed, which may have made room for those refused for it
    /// among `after` it (the lists subscribed after it: room is measured
    /// against the lists before one, so earlier ones gain none).
    pub(super) fn retry_lists_refused_for_room(&self, after: &[String]) {
        let refused: Vec<String> = {
            let cache = self.cache();
            after
                .iter()
                .filter_map(|id| cache.get(id))
                .filter(|s| refused_for_room(s))
                .map(|s| s.id.clone())
                .collect()
        };
        self.waiting_for_room().extend(refused);
    }
}

/// The ids, in order, of the lists whose hosts don't fit: each is measured
/// against the lists kept before it, and a cleared one adds nothing.
fn lists_past_the_limit(in_order: &[Subscription], cap: u64) -> Vec<String> {
    let mut kept: u64 = 0;
    let mut past = Vec::new();
    for sub in in_order {
        let hosts = u64::try_from(sub.entry_count).unwrap_or(0);
        if kept.saturating_add(hosts) > cap {
            past.push(sub.id.clone());
        } else {
            kept += hosts;
        }
    }
    past
}

/// The last download was refused by the limit on saved hosts.
pub(super) fn refused_for_room(sub: &Subscription) -> bool {
    matches!(&sub.last_fetch_status,
        FetchStatus::Failed { reason } if reason.starts_with(STORAGE_LIMIT_REASON_PREFIX))
}

/// Never attempted: due. Last attempt succeeded: due after the refresh
/// interval. Last attempt failed or never finished (the bridge stopped
/// mid-download): due after the retry backoff, except one refused for room,
/// which a download won't change until another list goes: that waits its
/// normal interval, so a 64 MiB list isn't fetched every hour to be refused
/// again.
fn is_due(sub: &Subscription, now: DateTime<Utc>) -> bool {
    let Some(attempt) = sub.last_attempt_at.or(sub.last_fetched_at) else {
        return true;
    };
    let succeeded = sub
        .last_fetched_at
        .filter(|fetched| *fetched >= attempt && sub.last_fetch_status == FetchStatus::Ok);
    match succeeded {
        Some(fetched) => (now - fetched).num_seconds() >= sub.refresh_interval_secs,
        None => {
            let wait = if refused_for_room(sub) {
                sub.refresh_interval_secs
            } else {
                FAILED_RETRY_SECS.min(sub.refresh_interval_secs)
            };
            (now - attempt).num_seconds() >= wait
        }
    }
}
