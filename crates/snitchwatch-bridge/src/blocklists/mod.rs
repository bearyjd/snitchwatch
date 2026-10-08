//! Bridge-owned blocklist subscriptions, fetch loop, and rule materialization.
//!
//! Production wiring (issue #45 PR A): one [`worker::BlocklistWorker`] runs
//! every subscribe, unsubscribe and scheduled refresh in order, so at most one
//! (bounded, https-only) fetch is in flight; [`spawn_event_pump`] turns
//! [`BlocklistEvent`]s into `ServerMessage`s. No daemon rules are installed
//! yet: the default [`NoopRuleSink`] reports every list as not enforced.

pub mod event_pump;
pub mod fetch_guard;
pub mod fetcher;
pub mod format;
mod manager;
pub mod materializer;
pub mod store;
pub mod worker;

pub use event_pump::spawn_event_pump;
pub use manager::{BlocklistsManager, SubscribeOutcome};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::blocklists::materializer::MaterializedRule;

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
    /// The sink accepted the rules: the daemon replied OK and (PR B) the list
    /// file was written. Shown as "Rule installed", not "Enforced": the daemon
    /// may still load 0 entries.
    RuleInstalled {
        at: DateTime<Utc>,
    },
    NotEnforced {
        reason: String,
    },
}

/// Why every list is unenforced until PR B wires a daemon sink. Shown to the
/// user.
pub const NO_RULE_SINK_REASON: &str = "Blocking isn't available yet";
/// Enforcement reason for a subscription whose first download failed.
pub const NOT_DOWNLOADED_REASON: &str = "The list hasn't been downloaded";
/// Fetch status reason when a downloaded list couldn't be written.
pub const STORE_ERROR_REASON: &str = "Couldn't save the list";
/// Most subscriptions one bridge keeps (each can hold `format::MAX_ENTRIES`).
pub const MAX_SUBSCRIPTIONS: usize = 32;
/// A list whose last download failed is retried after this long (or its own
/// refresh interval, if shorter), not on every scheduler tick.
pub const FAILED_RETRY_SECS: i64 = 60 * 60;
/// Sink that receives materialized deny rules after a successful blocklist
/// refresh. The default implementation is [`NoopRuleSink`]; replace it with
/// [`BlocklistsManager::with_rule_sink`] to wire in the real opensnitchd writer.
///
/// ## Replace contract (migration-critical)
///
/// `replace_blocklist_rules` has *replace* — not merely *add* — semantics: the
/// daemon's set of rules for `list_id` must end up **exactly** `rules`. An
/// implementation must therefore delete every existing rule whose name starts
/// with any prefix in
/// [`materializer::owned_blocklist_rule_name_prefixes`](crate::blocklists::materializer::owned_blocklist_rule_name_prefixes)
/// (that set spans both the current `"z00-blocklist:"` band and the legacy
/// `"900-blocklist:"` band) before installing `rules`. That is what migrates a
/// daemon off the old band: because the band move changes each rule's *name*,
/// the new rules do not overwrite the old ones by name, so the stale
/// old-prefix denies must be purged explicitly. See
/// [`materializer`](crate::blocklists::materializer)'s "Legacy band migration"
/// note. The `refresh_removes_legacy_band_rules` test exercises a sink that
/// honors this contract end-to-end.
// clippy 1.99's `double_must_use` fires on async_trait's generated
// `#[must_use]` methods (they return an already-must_use boxed future).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RuleSink: Send + Sync + 'static {
    /// False for a sink that installs nothing ([`NoopRuleSink`]). The manager
    /// then skips materializing rules (hundreds of bytes per host) and
    /// reports every list as [`NO_RULE_SINK_REASON`].
    fn installs_rules(&self) -> bool {
        true
    }

    async fn replace_blocklist_rules(
        &self,
        list_id: &str,
        rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()>;
}

/// Sink used when no real sink has been wired in. It installs nothing: it
/// says so through [`RuleSink::installs_rules`], and fails any push with
/// [`NO_RULE_SINK_REASON`]. The manager never reports such a list installed.
pub struct NoopRuleSink;

#[async_trait]
impl RuleSink for NoopRuleSink {
    fn installs_rules(&self) -> bool {
        false
    }

    async fn replace_blocklist_rules(
        &self,
        _list_id: &str,
        _rules: Vec<MaterializedRule>,
    ) -> anyhow::Result<()> {
        anyhow::bail!(NO_RULE_SINK_REASON)
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
