//! What the connected daemon refuses on top of the shapes
//! [`super::validate_operator`] refuses for every daemon.
//!
//! opensnitchd 1.9 (upstream master, `daemon/rule/operator.go` `Compile`)
//! compiles a `network` condition only with the `dest.network` operand;
//! v1.8.0, the shipped daemon, also takes `source.network`. A rule the
//! daemon won't compile is skipped when it loads its rules and answers
//! `CHANGE_RULE` with ERROR, so the bridge refuses to send one to a daemon
//! that reports 1.9 or newer. The version is the one the daemon's `ping`
//! reports (`Statistics.daemon_version`); `subscribe`'s `ClientConfig.version`
//! is the host's kernel release and is never used. Until the first `ping`
//! the version is unknown, and an unknown version keeps v1.8.0's rules: the
//! daemon's own refusal stays the backstop.
//!
//! Only rules being sent are checked: a rule the daemon already holds stays
//! editable (`read_only_reason` doesn't use a context).

use crate::daemon_config::DaemonVersion;

/// The first daemon version that compiles a `network` condition only with
/// `dest.network`.
pub const DEST_NETWORK_ONLY_SINCE: DaemonVersion = DaemonVersion::new(1, 9);

pub const SOURCE_NETWORK_REFUSED: &str = "opensnitchd 1.9 and newer can't match a \
     network condition on source.network; use dest.network instead.";

/// Daemon-dependent checks. The default is v1.8.0's: nothing extra.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PolicyContext {
    /// Refuse the `network` type with the `source.network` operand.
    pub refuse_source_network: bool,
}

impl PolicyContext {
    /// The checks that apply to one rule: the daemon compiles only enabled
    /// rules, so a disabled one gets what every daemon applies and nothing
    /// that depends on the version.
    pub fn for_rule(self, enabled: bool) -> Self {
        if enabled {
            self
        } else {
            Self::default()
        }
    }

    /// The checks for a daemon that reported `version` (`None`: unknown, as
    /// before its first `ping`; v1.8.0's checks).
    pub fn for_daemon(version: Option<DaemonVersion>) -> Self {
        Self {
            refuse_source_network: version.is_some_and(|v| v >= DEST_NETWORK_ONLY_SINCE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_policy::{
        check_wire_rule, check_wire_rule_in, read_only_reason, validate_operator,
        validate_operator_in, validate_user_rule, validate_user_rule_in, PolicyProfile,
    };
    use snitchwatch_proto::protocol::{Operator, Rule};

    const NEWER: PolicyContext = PolicyContext {
        refuse_source_network: true,
    };

    fn leaf(r#type: &str, operand: &str, data: &str) -> Operator {
        Operator {
            r#type: r#type.into(),
            operand: operand.into(),
            data: data.into(),
            ..Default::default()
        }
    }

    fn rule(operator: Operator) -> Rule {
        Rule {
            name: "lan-in".into(),
            enabled: true,
            action: "allow".into(),
            duration: "always".into(),
            operator: Some(operator),
            ..Default::default()
        }
    }

    #[test]
    fn source_network_is_refused_only_with_the_context() {
        let source = leaf("network", "source.network", "192.168.0.0/16");
        let dest = leaf("network", "dest.network", "192.168.0.0/16");
        assert_eq!(validate_operator(&source), Ok(()));
        assert_eq!(
            validate_operator_in(&source, PolicyContext::default()),
            Ok(())
        );
        assert_eq!(
            validate_operator_in(&source, NEWER),
            Err(SOURCE_NETWORK_REFUSED.to_string())
        );
        assert_eq!(validate_operator_in(&dest, NEWER), Ok(()));
        let listed = Operator {
            r#type: "list".into(),
            list: vec![leaf("simple", "dest.port", "22"), source.clone()],
            ..Default::default()
        };
        assert_eq!(validate_operator(&listed), Ok(()));
        assert_eq!(
            validate_operator_in(&listed, NEWER),
            Err(format!("list member 2: {SOURCE_NETWORK_REFUSED}"))
        );
        // Other shapes keep their own refusal.
        let simple = leaf("simple", "source.network", "192.168.0.0/16");
        assert_eq!(
            validate_operator_in(&simple, NEWER),
            validate_operator(&simple)
        );
        assert!(validate_operator(&simple).is_err());
    }

    #[test]
    fn whole_rules_are_refused_at_the_operator_and_stay_editable() {
        let source = rule(leaf("network", "source.network", "192.168.0.0/16"));
        for profile in [PolicyProfile::Editor, PolicyProfile::Import] {
            assert_eq!(validate_user_rule(&source, profile), Ok(()), "{profile:?}");
            let problems = validate_user_rule_in(&source, profile, NEWER).unwrap_err();
            assert_eq!(problems.len(), 1, "{profile:?}");
            assert_eq!(problems[0].path, "operator");
            assert_eq!(problems[0].reason, SOURCE_NETWORK_REFUSED);
            let dest = rule(leaf("network", "dest.network", "192.168.0.0/16"));
            assert_eq!(validate_user_rule_in(&dest, profile, NEWER), Ok(()));
        }
        // A rule the daemon holds is listed editable whatever its version.
        assert_eq!(read_only_reason(&source), None);
    }

    #[test]
    fn a_wire_rule_is_checked_for_the_daemon() {
        let wire = serde_json::json!({
            "name": "lan-in", "enabled": true, "action": "allow", "duration": "always",
            "operator": { "type": "network", "operand": "source.network", "data": "10.0.0.0/8" },
        });
        assert!(check_wire_rule(&wire, PolicyProfile::Editor).is_ok());
        let problems = check_wire_rule_in(&wire, PolicyProfile::Editor, NEWER).unwrap_err();
        assert_eq!(problems[0].reason, SOURCE_NETWORK_REFUSED);
        assert_eq!(
            SOURCE_NETWORK_REFUSED,
            "opensnitchd 1.9 and newer can't match a network condition on source.network; use \
             dest.network instead."
        );
    }

    #[test]
    fn only_a_known_1_9_or_newer_daemon_refuses_source_network() {
        let refuses = |version: Option<(u32, u32)>| {
            PolicyContext::for_daemon(
                version.map(|(major, minor)| DaemonVersion::new(major, minor)),
            )
            .refuse_source_network
        };
        assert!(!refuses(None));
        assert!(!refuses(Some((1, 6))));
        assert!(!refuses(Some((1, 8))));
        assert!(refuses(Some((1, 9))));
        assert!(refuses(Some((1, 10))));
        assert!(refuses(Some((2, 0))));
        assert_eq!(PolicyContext::default(), PolicyContext::for_daemon(None));
    }
}
