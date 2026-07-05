//! Helpers that build server-to-client [`ServerMessage`] variants for blocklist events.

use crate::blocklists::store::FetchStatus;
use crate::blocklists::BlocklistsManager;
use crate::ws_messages::{BlocklistEntry, BlocklistSummary, ServerMessage};

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
