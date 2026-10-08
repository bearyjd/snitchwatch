//! Profile messages from GUIs (issue #46): handled by the profile manager,
//! which only changes its store and asks for an enforcement pass, so the
//! pump never waits on the daemon here. An `AddProfileRule` with a valid
//! `request_id` gets one `RuleCommandResult` (P2.1's rule editor, in its
//! profile mode): ok once the rule is saved to the profile (installing it
//! is the enforcer's, shown as its status), or refused with the profile
//! policy's plain-text problems.

use std::sync::Arc;

use snitchwatch_bridge::profiles::{ProfilesError, ProfilesManager};
use snitchwatch_bridge::rule_policy::RuleProblem;
use snitchwatch_bridge::translator::upstream::handle_profile_action;
use snitchwatch_bridge::ws_messages::{
    valid_request_id, ClientMessage, RuleCommandOutcome, ServerMessage,
};
use tokio::sync::broadcast;
use tracing::error;

use crate::replier::Replier;

const GONE: &str = "This profile no longer exists.";
const NOT_SAVED: &str = "Snitchwatch couldn't save the rule to the profile.";

pub(crate) async fn handle(
    mgr: Arc<ProfilesManager>,
    msg: ClientMessage,
    broadcast: &broadcast::Sender<ServerMessage>,
) {
    let answer = match &msg {
        ClientMessage::AddProfileRule {
            request_id: Some(id),
            reply,
            ..
        } if valid_request_id(id) => {
            Some((id.clone(), Replier::new(reply.clone(), broadcast.clone())))
        }
        _ => None,
    };
    let result = handle_profile_action(mgr, msg).await;
    if let Some((request_id, replier)) = answer {
        let outcome = match &result {
            Ok(_) => RuleCommandOutcome::Ok,
            Err(e) => refusal(e),
        };
        // Answered at once, never by a task of its own.
        replier.send_now(ServerMessage::RuleCommandResult {
            request_id,
            outcome,
        });
    }
    if let Err(e) = result {
        error!(error = %e, "profile action failed");
    }
}

fn refusal(error: &anyhow::Error) -> RuleCommandOutcome {
    let problems = match error.downcast_ref::<ProfilesError>() {
        Some(ProfilesError::Refused(problems)) => problems.clone(),
        Some(ProfilesError::UnknownProfile(_)) => problem(GONE),
        _ => problem(NOT_SAVED),
    };
    RuleCommandOutcome::Refused { problems }
}

fn problem(reason: &str) -> Vec<RuleProblem> {
    vec![RuleProblem {
        path: "rule".into(),
        reason: reason.into(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::profiles::store::ProfileStore;
    use snitchwatch_bridge::ws_messages::ProfileRuleWire;

    fn add(host: &str, request_id: Option<&str>) -> ClientMessage {
        ClientMessage::AddProfileRule {
            profile_id: "home".into(),
            rule: ProfileRuleWire {
                id: "r1".into(),
                action: "deny".into(),
                operand: "dest.host".into(),
                data: host.into(),
                ..Default::default()
            },
            request_id: request_id.map(str::to_string),
            reply: None,
        }
    }

    fn result(rx: &mut broadcast::Receiver<ServerMessage>) -> Option<RuleCommandOutcome> {
        while let Ok(message) = rx.try_recv() {
            if let ServerMessage::RuleCommandResult { outcome, .. } = message {
                return Some(outcome);
            }
        }
        None
    }

    #[tokio::test]
    async fn an_added_profile_rule_is_answered_saved_or_refused() {
        let mgr = Arc::new(ProfilesManager::new(Arc::new(
            ProfileStore::open_in_memory().unwrap(),
        )));
        mgr.create_profile("home", "Home", vec![]).await.unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        handle(mgr.clone(), add("x.example", Some("p1")), &tx).await;
        assert_eq!(result(&mut rx), Some(RuleCommandOutcome::Ok));
        handle(mgr.clone(), add("", Some("p2")), &tx).await;
        match result(&mut rx) {
            Some(RuleCommandOutcome::Refused { problems }) => {
                assert!(problems.iter().any(|p| p.reason.contains("blank host")))
            }
            other => panic!("{other:?}"),
        }
        handle(mgr.clone(), add("y.example", None), &tx).await;
        assert_eq!(result(&mut rx), None, "no id, no result");
        let rules = mgr.store().get_profile("home").unwrap().unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].data, "y.example");
    }
}
