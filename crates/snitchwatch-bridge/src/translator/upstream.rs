//! Route WS ClientMessages from the UI into gRPC actions on the daemon.
//!
//! `apply` mutates the connection cache synchronously (resolving pending rows)
//! and returns an `UpstreamEffect` describing the side effect the bridge
//! orchestrator should perform against the gRPC client.

use std::sync::Arc;

use crate::blocklists::BlocklistsManager;
use crate::cache::connections::{ConnectionCache, Verdict};
use crate::ws_messages::{ClientMessage, VerdictAction};

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("cache error: {0}")]
    Cache(#[from] crate::cache::connections::CacheError),
}

/// Side effect the caller (the bridge orchestrator) should perform.
#[derive(Debug, Clone, PartialEq)]
pub enum UpstreamEffect {
    None,
    VerdictApplied {
        row_id: String,
        verdict: Verdict,
        remember: bool,
    },
    AddRule {
        rule: serde_json::Value,
    },
    DeleteRule {
        rule_id: String,
    },
    UpdateRule {
        rule_id: String,
        rule: serde_json::Value,
    },
}

/// Apply a ClientMessage to the bridge's state.
pub fn apply(
    cache: &mut ConnectionCache,
    msg: ClientMessage,
) -> Result<UpstreamEffect, RouterError> {
    match msg {
        ClientMessage::SetVerdict {
            row_id,
            verdict,
            scope: _,
            remember,
        } => {
            let v = match verdict {
                VerdictAction::Allow => Verdict::Allow,
                VerdictAction::Deny => Verdict::Deny,
            };
            cache.resolve(&row_id, v)?;
            Ok(UpstreamEffect::VerdictApplied {
                row_id,
                verdict: v,
                remember,
            })
        }
        ClientMessage::AddRule { rule } => Ok(UpstreamEffect::AddRule { rule }),
        ClientMessage::DeleteRule { rule_id } => Ok(UpstreamEffect::DeleteRule { rule_id }),
        ClientMessage::UpdateRule { rule_id, rule } => {
            Ok(UpstreamEffect::UpdateRule { rule_id, rule })
        }
        ClientMessage::GlobalSettings { .. }
        | ClientMessage::SubscribeBlocklist { .. }
        | ClientMessage::UnsubscribeBlocklist { .. }
        | ClientMessage::Undo
        | ClientMessage::Redo => Ok(UpstreamEffect::None),
    }
}

/// Outcome of routing a blocklist-related ClientMessage to the manager.
#[derive(Debug, PartialEq)]
pub enum BlocklistActionOutcome {
    Subscribed { id: String },
    Unsubscribed { id: String },
    Unhandled(Box<ClientMessage>),
}

/// Route a blocklist ClientMessage to the appropriate BlocklistsManager method.
pub async fn handle_blocklist_action(
    mgr: Arc<BlocklistsManager>,
    action: ClientMessage,
) -> anyhow::Result<BlocklistActionOutcome> {
    match action {
        ClientMessage::SubscribeBlocklist { url } => {
            let id = mgr.add_subscription(&url).await?;
            let _ = mgr.refresh_now(&id).await;
            Ok(BlocklistActionOutcome::Subscribed { id })
        }
        ClientMessage::UnsubscribeBlocklist { id } => {
            mgr.remove_subscription(&id).await?;
            Ok(BlocklistActionOutcome::Unsubscribed { id })
        }
        other => Ok(BlocklistActionOutcome::Unhandled(Box::new(other))),
    }
}

#[cfg(test)]
mod blocklist_action_tests {
    use super::*;
    use crate::blocklists::store::BlocklistStore;
    use crate::blocklists::BlocklistsManager;
    use crate::ws_messages::ClientMessage;
    use std::sync::Arc;

    fn manager() -> Arc<BlocklistsManager> {
        Arc::new(BlocklistsManager::new(Arc::new(
            BlocklistStore::open_in_memory().unwrap(),
        )))
    }

    #[tokio::test]
    async fn subscribe_blocklist_adds_subscription_to_store() {
        let mgr = manager();
        let action = ClientMessage::SubscribeBlocklist {
            url: "https://example.invalid/hosts.txt".into(),
        };
        handle_blocklist_action(mgr.clone(), action).await.unwrap();
        let subs = mgr.store().list_subscriptions().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].url, "https://example.invalid/hosts.txt");
    }

    #[tokio::test]
    async fn unsubscribe_blocklist_removes_subscription() {
        let mgr = manager();
        let id = mgr
            .add_subscription("https://example.invalid/hosts.txt")
            .await
            .unwrap();
        handle_blocklist_action(mgr.clone(), ClientMessage::UnsubscribeBlocklist { id })
            .await
            .unwrap();
        assert!(mgr.store().list_subscriptions().unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_blocklist_action_is_returned_unhandled() {
        let mgr = manager();
        let action = ClientMessage::Undo;
        let outcome = handle_blocklist_action(mgr.clone(), action).await.unwrap();
        assert_eq!(
            outcome,
            BlocklistActionOutcome::Unhandled(Box::new(ClientMessage::Undo))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws_messages::{ConnectionRow, VerdictScope};

    fn make_pending(cache: &mut ConnectionCache, id: &str) {
        // The returned verdict receiver is intentionally dropped: these tests
        // only exercise `upstream::apply`, not the oneshot round-trip.
        drop(cache.insert_pending(ConnectionRow {
            id: id.to_string(),
            process: "p".to_string(),
            process_path: None,
            dst_host: "h".to_string(),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }));
    }

    #[test]
    fn set_verdict_resolves_pending_row() {
        let mut cache = ConnectionCache::new(10);
        make_pending(&mut cache, "p1");
        let effect = apply(
            &mut cache,
            ClientMessage::SetVerdict {
                row_id: "p1".to_string(),
                verdict: VerdictAction::Allow,
                scope: VerdictScope::ThisHost,
                remember: false,
            },
        )
        .unwrap();
        match effect {
            UpstreamEffect::VerdictApplied {
                row_id,
                verdict,
                remember,
            } => {
                assert_eq!(row_id, "p1");
                assert_eq!(verdict, Verdict::Allow);
                assert!(!remember);
            }
            other => panic!("unexpected effect: {:?}", other),
        }
        assert!(cache.pending_ids().is_empty());
    }

    #[test]
    fn add_rule_returns_add_rule_effect() {
        let mut cache = ConnectionCache::new(10);
        let rule = serde_json::json!({"name": "block-everything"});
        let effect = apply(&mut cache, ClientMessage::AddRule { rule: rule.clone() }).unwrap();
        assert_eq!(effect, UpstreamEffect::AddRule { rule });
    }

    #[test]
    fn undo_is_noop() {
        let mut cache = ConnectionCache::new(10);
        let effect = apply(&mut cache, ClientMessage::Undo).unwrap();
        assert_eq!(effect, UpstreamEffect::None);
    }
}
