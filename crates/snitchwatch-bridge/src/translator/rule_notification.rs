//! Translate rule-editing [`UpstreamEffect`]s into the `Notification`s
//! opensnitchd acts on.
//!
//! This is the missing leg of rule enable/disable/delete: the effects were
//! produced and then dropped, because the bridge's outbound `Notifications`
//! stream was parked on `pending()`. `snitchwatch-bridge-cli` calls
//! [`notification_for_effect`] and pushes the result through
//! `UiService::notifications_handle`.
//!
//! **Why `CHANGE_RULE` for a toggle rather than `ENABLE_RULE`/`DISABLE_RULE`:**
//! all three daemon handlers end in `c.rules.Replace(r, r.Duration == Always)`
//! (`vendor/opensnitch/daemon/ui/notifications.go:87-126`); `ENABLE`/`DISABLE`
//! merely force `r.Enabled` before replacing. The Rules model already sends the
//! desired `enabled` value inside the rule (`RulesModel::toggle_enabled` ->
//! `RulesStore::toggled_rule_json`), so `CHANGE_RULE` expresses the whole
//! change in one action with no extra branch.
//!
//! **Persistence caveat** (surfaces to users, not a bug here): that second
//! `Replace` argument is "save to disk", so the daemon only persists a rule
//! whose duration is `always`. Toggling a `once`/`30s`/`until restart` rule
//! changes the daemon's in-memory rule and will not survive a daemon restart.

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use crate::translator::upstream::UpstreamEffect;

/// Build the daemon notification for a rule-editing effect.
///
/// Returns `Ok(None)` for effects that aren't rule edits (the caller keeps its
/// existing handling for those), and `Err` when a rule can't be represented in
/// the shape the daemon accepts — see [`crate::grpc_server::rule_from_wire`]
/// for why a malformed rule must die here rather than reach the daemon.
///
/// `id` is echoed back by the daemon in its `NotificationReply`, so callers
/// should pass a monotonically increasing, non-zero value.
pub fn notification_for_effect(
    effect: &UpstreamEffect,
    id: u64,
) -> Result<Option<Notification>, String> {
    let (action, rules) = match effect {
        // The daemon has no "add"; `Replace` creates a rule that doesn't exist
        // yet, so both add and update are CHANGE_RULE.
        UpstreamEffect::AddRule { rule } | UpstreamEffect::UpdateRule { rule, .. } => (
            Action::ChangeRule,
            vec![crate::grpc_server::rule_from_wire(rule)?],
        ),
        // DELETE_RULE reads only `rul.Name`
        // (`vendor/opensnitch/daemon/ui/notifications.go:132`), so a name-only
        // Rule is correct and complete here. Deliberately NOT routed through
        // `rule_from_wire`: there is no operator to validate, and requiring one
        // would make deleting a rule impossible.
        UpstreamEffect::DeleteRule { rule_id } => {
            if rule_id.is_empty() {
                return Err("DeleteRule with an empty rule id".to_string());
            }
            (
                Action::DeleteRule,
                vec![Rule {
                    name: rule_id.clone(),
                    ..Default::default()
                }],
            )
        }
        _ => return Ok(None),
    };

    Ok(Some(Notification {
        id,
        r#type: action as i32,
        rules,
        ..Default::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_rule(name: &str, enabled: bool) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "enabled": enabled,
            "action": "allow",
            "duration": "always",
            "description": "",
            "operator": {
                "type": "simple",
                "operand": "dest.host",
                "data": "example.com",
                "sensitive": false,
            },
        })
    }

    #[test]
    fn update_rule_becomes_change_rule_carrying_the_full_rule() {
        let effect = UpstreamEffect::UpdateRule {
            rule_id: "899-firefox".to_string(),
            rule: wire_rule("899-firefox", false),
        };
        let ntf = notification_for_effect(&effect, 7).unwrap().unwrap();

        assert_eq!(ntf.id, 7);
        assert_eq!(ntf.r#type, Action::ChangeRule as i32);
        assert_eq!(ntf.rules.len(), 1);
        assert_eq!(ntf.rules[0].name, "899-firefox");
        // The toggled `enabled` must survive: it is the entire point of the
        // notification, and CHANGE_RULE replaces the daemon's rule wholesale.
        assert!(!ntf.rules[0].enabled);
        let op = ntf.rules[0].operator.as_ref().expect("operator required");
        assert_eq!(op.operand, "dest.host");
    }

    #[test]
    fn add_rule_also_maps_to_change_rule() {
        let effect = UpstreamEffect::AddRule {
            rule: wire_rule("new-rule", true),
        };
        let ntf = notification_for_effect(&effect, 1).unwrap().unwrap();
        assert_eq!(ntf.r#type, Action::ChangeRule as i32);
        assert!(ntf.rules[0].enabled);
    }

    #[test]
    fn delete_rule_sends_a_name_only_rule() {
        let effect = UpstreamEffect::DeleteRule {
            rule_id: "z00-blocklist:ads:0001-x.example".to_string(),
        };
        let ntf = notification_for_effect(&effect, 2).unwrap().unwrap();

        assert_eq!(ntf.r#type, Action::DeleteRule as i32);
        assert_eq!(ntf.rules.len(), 1);
        assert_eq!(ntf.rules[0].name, "z00-blocklist:ads:0001-x.example");
        // No operator needed, and requiring one would make delete impossible.
        assert!(ntf.rules[0].operator.is_none());
    }

    #[test]
    fn a_rule_the_daemon_would_reject_is_an_error_not_a_notification() {
        // `operator: null` is the issue #14 failure mode: the daemon's
        // `rule.Deserialize` rejects it and silently applies its default
        // action. It must never leave the bridge.
        let mut bad = wire_rule("899-firefox", true);
        bad["operator"] = serde_json::Value::Null;
        let effect = UpstreamEffect::UpdateRule {
            rule_id: "899-firefox".to_string(),
            rule: bad,
        };
        assert!(notification_for_effect(&effect, 3).is_err());
    }

    /// The daemon's CHANGE_RULE handler does a wholesale `Replace`, so every
    /// field the wire shape drops is a field a toggle silently clears. For
    /// `precedence` that changes which rule wins for unrelated traffic — an
    /// invisible security-relevant side effect of an "enable/disable" click.
    ///
    /// Guards the bridge half of that round trip; `rules::row_store`'s
    /// `toggling_preserves_precedence_and_nolog_through_the_wire` guards the
    /// Rules-model half.
    #[test]
    fn precedence_and_nolog_survive_into_the_notification() {
        let mut rule = wire_rule("010-priority-allow", true);
        rule["precedence"] = serde_json::Value::Bool(true);
        rule["nolog"] = serde_json::Value::Bool(true);

        let effect = UpstreamEffect::UpdateRule {
            rule_id: "010-priority-allow".to_string(),
            rule,
        };
        let ntf = notification_for_effect(&effect, 9).unwrap().unwrap();

        assert!(
            ntf.rules[0].precedence,
            "precedence must reach the daemon; dropping it silently reorders rule evaluation"
        );
        assert!(ntf.rules[0].nolog, "nolog must reach the daemon");
    }

    #[test]
    fn empty_delete_id_is_rejected() {
        let effect = UpstreamEffect::DeleteRule {
            rule_id: String::new(),
        };
        assert!(notification_for_effect(&effect, 4).is_err());
    }

    #[test]
    fn non_rule_effects_produce_no_notification() {
        assert!(
            notification_for_effect(&UpstreamEffect::SnapshotRequested, 5)
                .unwrap()
                .is_none()
        );
        assert!(notification_for_effect(&UpstreamEffect::None, 6)
            .unwrap()
            .is_none());
    }

    #[test]
    fn notification_type_is_never_none() {
        // A NONE-typed notification tells the daemon to close the stream
        // (`notifications.go:405-408`). Nothing this function emits may be one.
        for effect in [
            UpstreamEffect::UpdateRule {
                rule_id: "r".to_string(),
                rule: wire_rule("r", true),
            },
            UpstreamEffect::DeleteRule {
                rule_id: "r".to_string(),
            },
            UpstreamEffect::AddRule {
                rule: wire_rule("r", true),
            },
        ] {
            let ntf = notification_for_effect(&effect, 1).unwrap().unwrap();
            assert_ne!(ntf.r#type, Action::None as i32);
        }
    }
}
