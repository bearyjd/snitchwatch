//! Optional bridge features a client may rely on, advertised in the
//! `authenticated` acknowledgement (`ServerMessage::Authenticated`).
//!
//! A client must treat a missing or empty list as "none of these": every
//! bridge before this list existed (v0.1.1 and the #39 system-bridge head)
//! sends a bare `{"action":"authenticated"}`. Unknown strings are ignored, so
//! a newer bridge can add features without breaking an older client.

/// "This host" verdicts with a remembered duration are bound to the asking
/// program (`process.path` AND `dest.host` / `dest.ip`, issue #44 / #50 / #71)
/// instead of covering every app. Older bridges built host-only rules, so a
/// client must not offer "block just this program" without it (plan
/// `docs/superpowers/plans/2026-10-08-inline-deny-until-restart.md`).
pub const APP_BOUND_RULES: &str = "appBoundRules";

/// What this bridge advertises to every authenticated client.
pub fn advertised() -> Vec<String> {
    vec![APP_BOUND_RULES.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws_messages::ServerMessage;
    use serde::Deserialize;

    /// The `ServerMessage` decoder of a v0.1.1 client (Kirigami's
    /// `await_authentication_ack`): same serde attributes, `Authenticated` a
    /// unit variant. It must keep accepting the acknowledgement.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(
        tag = "action",
        rename_all = "camelCase",
        rename_all_fields = "camelCase"
    )]
    enum V011ServerMessage {
        Authenticated,
        RemoveConnectionRows {
            #[allow(dead_code)]
            ids: Vec<String>,
        },
    }

    fn current_ack() -> String {
        serde_json::to_string(&ServerMessage::Authenticated {
            capabilities: advertised(),
        })
        .unwrap()
    }

    #[test]
    fn this_bridge_advertises_app_bound_rules() {
        let ack: serde_json::Value = serde_json::from_str(&current_ack()).unwrap();
        assert_eq!(
            ack,
            serde_json::json!({ "action": "authenticated", "capabilities": ["appBoundRules"] })
        );
    }

    #[test]
    fn a_v0_1_1_client_still_accepts_the_acknowledgement() {
        assert_eq!(
            serde_json::from_str::<V011ServerMessage>(&current_ack()).unwrap(),
            V011ServerMessage::Authenticated
        );
    }

    #[test]
    fn an_old_bridges_bare_acknowledgement_advertises_nothing() {
        assert_eq!(
            serde_json::from_str::<ServerMessage>(r#"{"action":"authenticated"}"#).unwrap(),
            ServerMessage::Authenticated {
                capabilities: Vec::new()
            }
        );
    }
}
