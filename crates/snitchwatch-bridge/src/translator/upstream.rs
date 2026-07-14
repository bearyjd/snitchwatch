//! Route WS ClientMessages from the UI into gRPC actions on the daemon.
//!
//! `apply` mutates the connection cache synchronously (resolving pending rows)
//! and returns an `UpstreamEffect` describing the side effect the bridge
//! orchestrator should perform against the gRPC client.

use std::sync::Arc;

use crate::blocklists::BlocklistsManager;
use crate::cache::connections::{ConnectionCache, Verdict};
use crate::profiles::store::ProfileRule;
use crate::profiles::ProfilesManager;
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
    /// A feed consumer lagged and asked for full state snapshots to be
    /// re-broadcast. The orchestrator answers with the current connection
    /// rows plus blocklist/profile snapshots (see `snitchwatch-bridge-cli`).
    SnapshotRequested,
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
            duration,
            remember: legacy_remember,
        } => {
            let v = match verdict {
                VerdictAction::Allow => Verdict::Allow,
                VerdictAction::Deny => Verdict::Deny,
            };
            let duration =
                crate::ws_messages::effective_verdict_duration(duration, legacy_remember);
            let remember = duration.remembers();
            cache.resolve(&row_id, v, duration)?;
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
        ClientMessage::RequestSnapshot => Ok(UpstreamEffect::SnapshotRequested),
        // SetFilteringPaused is intercepted earlier in the pump loop
        // (snitchwatch-bridge-cli::run, mirroring how profile messages are
        // special-cased before reaching here) since it toggles a shared
        // flag + tray state, not cache state this function owns.
        ClientMessage::GlobalSettings { .. }
        | ClientMessage::SubscribeBlocklist { .. }
        | ClientMessage::UnsubscribeBlocklist { .. }
        | ClientMessage::CreateProfile { .. }
        | ClientMessage::UpdateProfile { .. }
        | ClientMessage::DeleteProfile { .. }
        | ClientMessage::ActivateProfile { .. }
        | ClientMessage::DeactivateProfile
        | ClientMessage::AddProfileRule { .. }
        | ClientMessage::RemoveProfileRule { .. }
        | ClientMessage::Undo
        | ClientMessage::Redo
        | ClientMessage::SetFilteringPaused { .. }
        | ClientMessage::RecheckDiagnostics => Ok(UpstreamEffect::None),
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

/// Outcome of routing a profile-related ClientMessage to the manager.
#[derive(Debug, PartialEq)]
pub enum ProfileActionOutcome {
    Created { id: String },
    Updated { id: String },
    Deleted { id: String },
    Activated { id: String },
    Deactivated,
    RuleAdded { profile_id: String },
    RuleRemoved { profile_id: String },
    Unhandled(Box<ClientMessage>),
}

/// Route a profile ClientMessage to the appropriate ProfilesManager method.
pub async fn handle_profile_action(
    mgr: Arc<ProfilesManager>,
    action: ClientMessage,
) -> anyhow::Result<ProfileActionOutcome> {
    match action {
        ClientMessage::CreateProfile {
            id,
            name,
            network_matchers,
        } => {
            mgr.create_profile(&id, &name, network_matchers).await?;
            Ok(ProfileActionOutcome::Created { id })
        }
        ClientMessage::UpdateProfile {
            id,
            name,
            network_matchers,
        } => {
            mgr.update_profile(&id, &name, network_matchers).await?;
            Ok(ProfileActionOutcome::Updated { id })
        }
        ClientMessage::DeleteProfile { id } => {
            mgr.delete_profile(&id).await?;
            Ok(ProfileActionOutcome::Deleted { id })
        }
        ClientMessage::ActivateProfile { id } => {
            mgr.activate(&id).await?;
            Ok(ProfileActionOutcome::Activated { id })
        }
        ClientMessage::DeactivateProfile => {
            mgr.deactivate().await?;
            Ok(ProfileActionOutcome::Deactivated)
        }
        ClientMessage::AddProfileRule { profile_id, rule } => {
            mgr.add_rule(
                &profile_id,
                ProfileRule {
                    id: rule.id,
                    action: rule.action,
                    operand: rule.operand,
                    data: rule.data,
                },
            )
            .await?;
            Ok(ProfileActionOutcome::RuleAdded { profile_id })
        }
        ClientMessage::RemoveProfileRule {
            profile_id,
            rule_id,
        } => {
            mgr.remove_rule(&profile_id, &rule_id).await?;
            Ok(ProfileActionOutcome::RuleRemoved { profile_id })
        }
        other => Ok(ProfileActionOutcome::Unhandled(Box::new(other))),
    }
}

#[cfg(test)]
mod profile_action_tests {
    use super::*;
    use crate::profiles::store::ProfileStore;
    use crate::ws_messages::ProfileRuleWire;
    use std::sync::Arc;

    fn manager() -> Arc<ProfilesManager> {
        Arc::new(ProfilesManager::new(Arc::new(
            ProfileStore::open_in_memory().unwrap(),
        )))
    }

    #[tokio::test]
    async fn create_profile_adds_to_store() {
        let mgr = manager();
        let action = ClientMessage::CreateProfile {
            id: "home".into(),
            name: "At Home".into(),
            network_matchers: vec!["Home*".into()],
        };
        handle_profile_action(mgr.clone(), action).await.unwrap();
        let profiles = mgr.store().list_profiles().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "home");
    }

    #[tokio::test]
    async fn activate_and_deactivate_profile_round_trip() {
        let mgr = manager();
        handle_profile_action(
            mgr.clone(),
            ClientMessage::CreateProfile {
                id: "home".into(),
                name: "Home".into(),
                network_matchers: vec![],
            },
        )
        .await
        .unwrap();

        handle_profile_action(
            mgr.clone(),
            ClientMessage::ActivateProfile { id: "home".into() },
        )
        .await
        .unwrap();
        assert_eq!(mgr.store().get_active().unwrap().unwrap().id, "home");

        handle_profile_action(mgr.clone(), ClientMessage::DeactivateProfile)
            .await
            .unwrap();
        assert!(mgr.store().get_active().unwrap().is_none());
    }

    #[tokio::test]
    async fn add_and_remove_profile_rule() {
        let mgr = manager();
        handle_profile_action(
            mgr.clone(),
            ClientMessage::CreateProfile {
                id: "home".into(),
                name: "Home".into(),
                network_matchers: vec![],
            },
        )
        .await
        .unwrap();

        handle_profile_action(
            mgr.clone(),
            ClientMessage::AddProfileRule {
                profile_id: "home".into(),
                rule: ProfileRuleWire {
                    id: "r1".into(),
                    action: "deny".into(),
                    operand: "dest.host".into(),
                    data: "ads.example".into(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(
            mgr.store()
                .get_profile("home")
                .unwrap()
                .unwrap()
                .rules
                .len(),
            1
        );

        handle_profile_action(
            mgr.clone(),
            ClientMessage::RemoveProfileRule {
                profile_id: "home".into(),
                rule_id: "r1".into(),
            },
        )
        .await
        .unwrap();
        assert!(mgr
            .store()
            .get_profile("home")
            .unwrap()
            .unwrap()
            .rules
            .is_empty());
    }

    #[tokio::test]
    async fn non_profile_action_is_returned_unhandled() {
        let mgr = manager();
        let outcome = handle_profile_action(mgr.clone(), ClientMessage::Undo)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ProfileActionOutcome::Unhandled(Box::new(ClientMessage::Undo))
        );
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
    use crate::ws_messages::{ConnectionRow, VerdictDuration, VerdictScope};

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
            matched_rule: None,
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
                duration: Some(VerdictDuration::Once),
                remember: None,
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
    fn set_verdict_with_a_persistent_duration_reports_remember_true() {
        let mut cache = ConnectionCache::new(10);
        make_pending(&mut cache, "p1");
        let effect = apply(
            &mut cache,
            ClientMessage::SetVerdict {
                row_id: "p1".to_string(),
                verdict: VerdictAction::Deny,
                scope: VerdictScope::ThisHost,
                duration: Some(VerdictDuration::Always),
                remember: None,
            },
        )
        .unwrap();
        match effect {
            UpstreamEffect::VerdictApplied { remember, .. } => assert!(remember),
            other => panic!("unexpected effect: {:?}", other),
        }
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
