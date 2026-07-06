//! Helpers that build server-to-client [`ServerMessage`] variants for
//! blocklist and profile events.

use crate::blocklists::store::FetchStatus;
use crate::blocklists::BlocklistsManager;
use crate::profiles::ProfilesManager;
use crate::ws_messages::{
    BlocklistEntry, BlocklistSummary, ProfileRuleWire, ProfileSummary, ServerMessage,
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
            BlocklistSummary {
                id: s.id,
                display_name: s.display_name,
                url: s.url,
                entry_count: s.entry_count,
                status,
                last_updated_iso8601: s.last_fetched_at.map(|t| t.to_rfc3339()),
                last_failure_reason,
            }
        })
        .collect();
    Ok(ServerMessage::SetBlocklists { blocklists })
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
            ServerMessage::SetBlocklists { blocklists } => {
                assert_eq!(blocklists.len(), 2);
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
}
