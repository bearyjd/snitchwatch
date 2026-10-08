//! Helpers that build server-to-client [`ServerMessage`] variants for
//! blocklist and profile events.

use crate::blocklists::fetcher::MAX_URL_LEN;
use crate::blocklists::store::FetchStatus;
use crate::blocklists::{derive_display_name, derive_id, BlocklistsManager, Enforcement};
use crate::profiles::ProfilesManager;
use crate::ws_messages::{
    BlocklistEntry, BlocklistSummary, ProfileRuleWire, ProfileSummary, ServerMessage,
    ENFORCEMENT_NOT_ENFORCED, ENFORCEMENT_PENDING, ENFORCEMENT_RULE_INSTALLED,
};

pub async fn build_set_blocklists(mgr: &BlocklistsManager) -> anyhow::Result<ServerMessage> {
    let subs = mgr.store().list_subscriptions()?;
    let blocklists = subs
        .into_iter()
        .map(|s| {
            let (status, last_failure_reason) = match s.last_fetch_status {
                FetchStatus::Pending => ("pending".to_string(), None),
                FetchStatus::Ok => ("ok".to_string(), None),
                FetchStatus::Failed { reason } => ("failed".to_string(), Some(reason)),
            };
            let (enforcement, enforcement_reason) = enforcement_wire(mgr.enforcement(&s.id));
            BlocklistSummary {
                id: s.id,
                display_name: s.display_name,
                url: s.url,
                entry_count: s.entry_count,
                status,
                last_updated_iso8601: s.last_fetched_at.map(|t| t.to_rfc3339()),
                last_failure_reason,
                enforcement,
                enforcement_reason,
            }
        })
        .collect();
    Ok(ServerMessage::SetBlocklists {
        blocklists,
        storage: Some(mgr.storage_status().clone()),
    })
}

fn enforcement_wire(enforcement: Enforcement) -> (String, Option<String>) {
    match enforcement {
        Enforcement::Pending => (ENFORCEMENT_PENDING.to_string(), None),
        Enforcement::RuleInstalled { .. } => (ENFORCEMENT_RULE_INSTALLED.to_string(), None),
        Enforcement::NotEnforced { reason } => (ENFORCEMENT_NOT_ENFORCED.to_string(), Some(reason)),
    }
}

/// A row for a subscribe request that was refused and never stored, so the
/// user sees why. The next `SetBlocklists` drops it.
pub fn build_rejected_blocklist(url: &str, reason: &str) -> ServerMessage {
    ServerMessage::SetBlocklistDetails {
        details: BlocklistSummary {
            id: derive_id(url),
            display_name: derive_display_name(url),
            url: url.chars().take(MAX_URL_LEN).collect(),
            entry_count: 0,
            status: "failed".to_string(),
            last_updated_iso8601: None,
            last_failure_reason: Some(reason.to_string()),
            enforcement: ENFORCEMENT_NOT_ENFORCED.to_string(),
            enforcement_reason: Some(reason.to_string()),
        },
    }
}

pub async fn build_set_blocklist_entries(
    mgr: &BlocklistsManager,
    subscription_id: &str,
) -> anyhow::Result<ServerMessage> {
    let hosts = mgr.store().list_entries(subscription_id)?;
    let entries = hosts
        .into_iter()
        .map(|host| BlocklistEntry { host })
        .collect();
    Ok(ServerMessage::SetBlocklistEntries {
        subscription_id: subscription_id.to_string(),
        entries,
    })
}

pub async fn build_set_blocklist_status(
    mgr: &BlocklistsManager,
    subscription_id: &str,
) -> anyhow::Result<ServerMessage> {
    let sub = mgr
        .store()
        .get_subscription(subscription_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown subscription: {subscription_id}"))?;
    let (status, last_failure_reason) = match sub.last_fetch_status {
        FetchStatus::Pending => ("pending".to_string(), None),
        FetchStatus::Ok => ("ok".to_string(), None),
        FetchStatus::Failed { reason } => ("failed".to_string(), Some(reason)),
    };
    Ok(ServerMessage::SetBlocklistStatus {
        subscription_id: sub.id,
        status,
        last_failure_reason,
    })
}

pub async fn build_set_profiles(mgr: &ProfilesManager) -> anyhow::Result<ServerMessage> {
    let profiles = mgr
        .store()
        .list_profiles()?
        .into_iter()
        .map(|p| ProfileSummary {
            id: p.id,
            name: p.name,
            network_matchers: p.network_matchers,
            rules: p
                .rules
                .into_iter()
                .map(|r| ProfileRuleWire {
                    id: r.id,
                    action: r.action,
                    operand: r.operand,
                    data: r.data,
                })
                .collect(),
            active: p.active,
        })
        .collect();
    Ok(ServerMessage::SetProfiles { profiles })
}

pub fn build_profile_changed(active_profile_id: Option<String>) -> ServerMessage {
    ServerMessage::ProfileChanged { active_profile_id }
}

#[cfg(test)]
mod profile_emission_tests {
    use super::*;
    use crate::profiles::store::{Profile, ProfileStore};
    use std::sync::Arc;

    #[tokio::test]
    async fn profiles_changed_yields_set_profiles() {
        let store = Arc::new(ProfileStore::open_in_memory().unwrap());
        store
            .upsert_profile(&Profile {
                id: "home".into(),
                name: "At Home".into(),
                network_matchers: vec!["Home*".into()],
                rules: vec![],
                active: true,
            })
            .unwrap();
        let mgr = ProfilesManager::new(store);
        let msg = build_set_profiles(&mgr).await.unwrap();
        match msg {
            ServerMessage::SetProfiles { profiles } => {
                assert_eq!(profiles.len(), 1);
                assert_eq!(profiles[0].id, "home");
                assert!(profiles[0].active);
            }
            other => panic!("expected SetProfiles, got {other:?}"),
        }
    }

    #[test]
    fn profile_changed_carries_active_id() {
        let msg = build_profile_changed(Some("home".into()));
        assert_eq!(
            msg,
            ServerMessage::ProfileChanged {
                active_profile_id: Some("home".into())
            }
        );
    }
}

#[cfg(test)]
mod blocklist_emission_tests {
    use super::*;
    use crate::blocklists::test_helpers::seeded_manager;

    #[tokio::test]
    async fn subscriptions_changed_yields_set_blocklists() {
        let mgr = seeded_manager(&[("stevenblack", 5), ("easylist", 3)]);
        let msg = build_set_blocklists(&mgr).await.unwrap();
        match msg {
            ServerMessage::SetBlocklists {
                blocklists,
                storage,
            } => {
                assert_eq!(
                    storage.map(|s| s.persistent),
                    Some(false),
                    "the default manager is not persistent"
                );
                assert_eq!(blocklists.len(), 2);
                assert!(blocklists
                    .iter()
                    .all(|b| b.enforcement == ENFORCEMENT_PENDING));
                assert!(blocklists
                    .iter()
                    .any(|b| b.id == "stevenblack" && b.entry_count == 5));
                assert!(blocklists
                    .iter()
                    .any(|b| b.id == "easylist" && b.entry_count == 3));
            }
            other => panic!("expected SetBlocklists, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn entries_changed_yields_set_blocklist_entries() {
        let mgr = seeded_manager(&[("test", 2)]);
        let msg = build_set_blocklist_entries(&mgr, "test").await.unwrap();
        match msg {
            ServerMessage::SetBlocklistEntries {
                subscription_id,
                entries,
            } => {
                assert_eq!(subscription_id, "test");
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected SetBlocklistEntries, got {other:?}"),
        }
    }

    #[test]
    fn a_rejected_url_becomes_a_failed_not_enforced_row() {
        let long = format!("http://x.example/{}", "a".repeat(10_000));
        match build_rejected_blocklist(&long, "only https:// blocklist URLs are allowed") {
            ServerMessage::SetBlocklistDetails { details } => {
                assert_eq!(details.status, "failed");
                assert_eq!(details.enforcement, ENFORCEMENT_NOT_ENFORCED);
                assert_eq!(
                    details.last_failure_reason.as_deref(),
                    Some("only https:// blocklist URLs are allowed")
                );
                assert!(details.url.len() <= MAX_URL_LEN, "the echoed URL is capped");
            }
            other => panic!("expected SetBlocklistDetails, got {other:?}"),
        }
    }
}
