//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.
//!
//! Production wiring (issue #45): one [`worker::BlocklistWorker`] runs every
//! subscribe, unsubscribe, scheduled refresh and reconcile in order, so at
//! most one (bounded, https-only) fetch is in flight; [`spawn_event_pump`]
//! turns [`BlocklistEvent`]s into `ServerMessage`s. With a state directory,
//! [`daemon_sink::DaemonRuleSink`] installs each list as opensnitchd
//! `lists.*` deny rules over files in [`list_dir`]; without one, the
//! [`NoopRuleSink`] reports every list as not enforced, and why.

pub mod daemon_sink;
pub mod event_pump;
pub mod fetch_guard;
pub mod fetcher;
pub mod format;
pub mod leftover;
pub mod list_dir;
mod manager;
pub mod materializer;
pub mod store;
pub mod worker;

pub use event_pump::spawn_event_pump;
pub use manager::{BlocklistsManager, SubscribeOutcome};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Events emitted whenever blocklist state changes. [`spawn_event_pump`]
/// rebroadcasts them as `SetBlocklists` / `SetBlocklistEntries` / … over the WS.
#[derive(Debug, Clone)]
pub enum BlocklistEvent {
    SubscriptionsChanged,
    EntriesChanged {
        subscription_id: String,
    },
    StatusChanged {
        subscription_id: String,
    },
    /// A subscribe request was refused before anything was stored (a bad URL,
    /// too many lists, or the worker queue was full). Shown to the user,
    /// never persisted.
    SubscriptionRejected {
        url: String,
        reason: String,
    },
    /// A GUI asked for a page of a subscription's hosts. Entries are only
    /// ever sent a page at a time, on request (issue #45).
    EntriesRequested {
        subscription_id: String,
        offset: u64,
        limit: u32,
    },
}

/// Whether a subscription's hosts are enforced by the daemon. Kept in memory
/// only; [`FetchStatus`] separately records the download result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforcement {
    /// Not downloaded or pushed to a sink yet.
    Pending,
    /// The list files were written and the daemon replied OK to every rule,
    /// or its committed rule snapshot already held each one unchanged.
    /// Shown as "Rule installed", not "Enforced": the daemon may still load
    /// 0 entries (a wrong path, SELinux) without saying so.
    RuleInstalled {
        at: DateTime<Utc>,
    },
    NotEnforced {
        reason: String,
    },
    /// Not known either way: the daemon's rule list is unknown or it didn't
    /// answer. Shown as "Not confirmed yet", with the reason.
    Unconfirmed {
        reason: String,
    },
}

/// Why a list is unenforced when no sink was wired in at all (in-process
/// test helpers). Shown to the user.
pub const NO_RULE_SINK_REASON: &str = "Blocking isn't available yet";
/// Enforcement reason for a subscription whose first download failed.
pub const NOT_DOWNLOADED_REASON: &str = "The list hasn't been downloaded";
/// Enforcement reason for a downloaded list with nothing a rule can match.
pub const NO_HOSTS_REASON: &str = "The list has no hosts Snitchwatch can block";
/// Enforcement reason for every list while the saved subscriptions can't be
/// read: the size limit can't count the rules already installed, so nothing
/// is installed or removed.
pub const UNREADABLE_STORE_REASON: &str = "Snitchwatch couldn't read its saved blocklists, so it \
     isn't changing any blocklist rules the firewall already has.";
/// How the reason of a list past [`AGGREGATE_MAX_HOSTS`] starts (GUIs key a
/// warning on it).
pub const OVER_LIMIT_REASON_PREFIX: &str = "Over the total blocklist size limit";
/// Enforcement reason for every list of a per-user bridge (GUIs key a
/// warning on it). Root opensnitchd would read list files any of the user's
/// processes could replace (a FIFO hangs it; a link to `/dev/zero` exhausts
/// its memory, and with `QueueBypass` the firewall fails open on every boot
/// after).
pub const PER_USER_REASON: &str = "Blocking with lists needs the system-wide Snitchwatch \
     service; this per-user service can't keep the list files safe from other apps.";

/// `2000000` as `2,000,000`.
pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
/// Fetch status reason when a downloaded list couldn't be written.
pub const STORE_ERROR_REASON: &str = "Couldn't save the list";
/// Most subscriptions one bridge keeps (each can hold `format::MAX_ENTRIES`).
pub const MAX_SUBSCRIPTIONS: usize = 32;
/// Most hosts, summed over every subscription, the bridge asks opensnitchd
/// to hold. The root daemon keeps each `lists` rule's entries in an
/// in-memory Go map, at roughly 100 bytes per host with map overhead, so
/// this is a ~200 MB budget. It runs with `QueueBypass`, so an OOM kill
/// would let traffic through unfiltered while it restarts. Lists past the
/// limit, in the order they were subscribed, get no files and no rule.
pub const AGGREGATE_MAX_HOSTS: u64 = 2_000_000;

/// How much a [`BlocklistsManager::reconcile_with`] pass does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReconcileScope {
    /// After a subscribe or unsubscribe: install lists not tried yet in
    /// this run, and remove orphans. A list just tried isn't tried again.
    CleanUp,
    /// After a committed daemon rules snapshot or a refresh tick: also
    /// retry every list that isn't installed (refused, timed out).
    Full,
}
/// A list whose last download failed is retried after this long (or its own
/// refresh interval, if shorter), not on every scheduler tick.
pub const FAILED_RETRY_SECS: i64 = 60 * 60;
/// How long, in minutes, a refresh tick leaves a list the daemon refused
/// alone: after the first refusal, the second, and every one after (issue
/// #73). A new daemon rule list or a new download tries again at once.
pub const REFUSAL_BACKOFF_MINUTES: [i64; 3] = [15, 60, 240];
/// Why a sink didn't install a list's rules. Shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotInstalled {
    pub reason: String,
    /// The daemon is unreachable or didn't answer: a reconcile pass stops
    /// here instead of trying every other list too.
    pub daemon_unavailable: bool,
}

impl NotInstalled {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            daemon_unavailable: false,
        }
    }

    pub fn daemon_unavailable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            daemon_unavailable: true,
        }
    }
}

/// Where a subscription's hosts go to be enforced (issue #45 PR B). The
/// production implementation is [`daemon_sink::DaemonRuleSink`]: one
/// `lists.*` deny rule per list kind, pointing at files it writes. The
/// default is [`NoopRuleSink`], which installs nothing and says why.
///
/// `replace_blocklist_rules` has *replace* semantics: afterwards the daemon
/// holds exactly the list's current rules (a stale kind's rule, or a legacy
/// per-host rule, is deleted).
// clippy 1.99's `double_must_use` fires on async_trait's generated
// `#[must_use]` methods (they return an already-must_use boxed future).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RuleSink: Send + Sync + 'static {
    /// `Some(reason)` for a sink that installs nothing ([`NoopRuleSink`]):
    /// every list is reported not enforced with that reason, and the
    /// manager never pushes to it or reconciles.
    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    /// Whether the daemon's rule list is known (a committed snapshot).
    /// Reconcile never runs while it isn't.
    fn daemon_rules_known(&self) -> bool {
        true
    }

    /// Whether `list_id`'s rules are in place with their files (the daemon's
    /// committed snapshot holds each unchanged): nothing to reconcile.
    fn is_current(&self, _list_id: &str) -> bool {
        false
    }

    /// Whether `list_id`'s files were written or checked against its hosts
    /// in this run and are still there: its rules can then be resent with
    /// [`reinstall_blocklist_rules`](Self::reinstall_blocklist_rules),
    /// without reading its hosts again.
    fn files_verified(&self, _list_id: &str) -> bool {
        false
    }

    /// Resend `list_id`'s rules over its checked files (see
    /// [`files_verified`](Self::files_verified)).
    async fn reinstall_blocklist_rules(&self, _list_id: &str) -> Result<(), NotInstalled> {
        Err(NotInstalled::new(NO_RULE_SINK_REASON))
    }

    /// Install `hosts` for `list_id`, replacing what was there. `Ok` only
    /// once the daemon confirmed every rule (or already had them, confirmed
    /// earlier in this run).
    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        hosts: Vec<String>,
    ) -> Result<(), NotInstalled>;

    /// Delete `list_id`'s rules from the daemon, then its files.
    async fn remove_blocklist_rules(&self, _list_id: &str) -> Result<(), NotInstalled> {
        Ok(())
    }

    /// Delete every blocklist rule and directory that doesn't belong to one
    /// of `keep`.
    async fn remove_orphans(&self, _keep: &[String]) {}
}

/// Sink used when no rule can be installed: it says why through
/// [`RuleSink::unavailable_reason`] and refuses every push with that reason.
pub struct NoopRuleSink {
    reason: String,
}

impl NoopRuleSink {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Default for NoopRuleSink {
    fn default() -> Self {
        Self::new(NO_RULE_SINK_REASON)
    }
}

#[async_trait]
impl RuleSink for NoopRuleSink {
    fn unavailable_reason(&self) -> Option<String> {
        Some(self.reason.clone())
    }

    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _hosts: Vec<String>,
    ) -> Result<(), NotInstalled> {
        Err(NotInstalled::new(self.reason.clone()))
    }
}

/// Longest sanitized stem kept in an id. Ids become rule names and (PR B) list
/// directory names, so a 2 KiB URL must not produce a 2 KiB id.
const MAX_ID_STEM_CHARS: usize = 64;

/// `<sanitized stem>-<16 hex (64 bits) of SHA-256(url)>`. The hash keeps two
/// URLs that end in the same file name (`…/hosts`) apart; the result only ever
/// contains `[A-Za-z0-9_-]`. A collision is still refused at subscribe time.
pub(crate) fn derive_id(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(url.as_bytes());
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{hash}", sanitized_stem(url))
}

pub(crate) fn derive_display_name(url: &str) -> String {
    sanitized_stem(url).replace('_', " ")
}

/// The URL's last path segment without query or `.txt`, with every character
/// outside `[A-Za-z0-9_-]` replaced by `_`, capped, or `list` if empty.
fn sanitized_stem(url: &str) -> String {
    let stem = url
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".txt");
    let cleaned: String = stem
        .chars()
        .take(MAX_ID_STEM_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "list".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
pub mod test_helpers {
    use crate::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
    use crate::blocklists::store::{BlocklistStore, FetchStatus, Subscription};
    use crate::blocklists::BlocklistsManager;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const FIXTURE_PREFIX: &str = "https://fixtures.invalid/";
    const FIXTURE_MAX_BYTES: u64 = 1024 * 1024;

    /// An https URL the [`FixtureFetcher`] serves from
    /// `tests/fixtures/blocklists/<name>`.
    pub fn fixture_url(name: &str) -> String {
        format!("{FIXTURE_PREFIX}{name}")
    }

    /// Test-only [`BlocklistFetch`]: reads `tests/fixtures/blocklists/<name>`
    /// for [`fixture_url`] URLs (size-capped), fails everything else. Counts
    /// calls. Production has no file-reading fetch path; this lives here.
    #[derive(Default)]
    pub struct FixtureFetcher {
        pub calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl BlocklistFetch for FixtureFetcher {
        async fn fetch(&self, url: &str) -> FetchOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let Some(name) = url.strip_prefix(FIXTURE_PREFIX) else {
                return FetchOutcome::Failed {
                    reason: format!("not a fixture URL: {url}"),
                };
            };
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/blocklists")
                .join(name);
            match std::fs::metadata(&path) {
                Ok(m) if m.len() <= FIXTURE_MAX_BYTES => {}
                Ok(m) => {
                    return FetchOutcome::Failed {
                        reason: format!("fixture too large: {} bytes", m.len()),
                    }
                }
                Err(e) => {
                    return FetchOutcome::Failed {
                        reason: format!("fixture {name}: {e}"),
                    }
                }
            }
            match std::fs::read_to_string(&path) {
                Ok(body) => process_body(&body),
                Err(e) => FetchOutcome::Failed {
                    reason: format!("fixture {name}: {e}"),
                },
            }
        }
    }

    pub fn seeded_manager(seeds: &[(&str, usize)]) -> BlocklistsManager {
        let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
        for (id, n_entries) in seeds {
            store
                .upsert_subscription(&Subscription {
                    id: (*id).to_string(),
                    url: format!("https://example.invalid/{id}.txt"),
                    display_name: (*id).to_string(),
                    format_hint: None,
                    refresh_interval_secs: 86_400,
                    last_fetched_at: None,
                    last_attempt_at: None,
                    last_fetch_status: FetchStatus::Ok,
                    entry_count: *n_entries as i64,
                })
                .unwrap();
            let hosts: Vec<String> = (0..*n_entries)
                .map(|i| format!("host{i}.{id}.example"))
                .collect();
            let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
            store.replace_entries(id, &host_refs).unwrap();
        }
        BlocklistsManager::new(store)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reconcile_tests;

#[cfg(test)]
mod backoff_tests;

#[cfg(test)]
mod cap_tests;

#[cfg(test)]
mod leftover_manager_tests;
