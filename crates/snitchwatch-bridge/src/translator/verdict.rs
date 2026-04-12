//! Translate a user-supplied `Verdict` (allow / deny) into the `Rule` proto
//! shape opensnitchd expects as the `AskRule` reply.
//!
//! The M0 spike taught us three things about `Rule`:
//!   - `name` must be non-empty (the daemon rejects empty names).
//!   - `created` is a unix-seconds int64.
//!   - `duration: "once"` means "this connection only" — the daemon does not
//!     persist the rule. Higher milestones (M2+) replace this with proper
//!     scope/remember handling.

use crate::cache::connections::Verdict;
use snitchwatch_proto::protocol::{Connection, Rule};

pub fn verdict_to_rule(verdict: Verdict, conn: &Connection, now_secs: i64) -> Rule {
    let action = match verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
    };

    let host = if conn.dst_host.is_empty() {
        conn.dst_ip.as_str()
    } else {
        conn.dst_host.as_str()
    };

    Rule {
        created: now_secs,
        name: format!("snitchwatch-{action}-{host}-{}", conn.dst_port),
        description: "snitchwatch interactive verdict".to_string(),
        enabled: true,
        precedence: false,
        nolog: false,
        action: action.to_string(),
        duration: "once".to_string(),
        operator: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            dst_ip: "140.82.121.4".to_string(),
            dst_host: "github.com".to_string(),
            dst_port: 443,
            process_path: "/usr/bin/curl".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn allow_verdict_produces_allow_rule_with_once_duration() {
        let rule = verdict_to_rule(Verdict::Allow, &sample_connection(), 1_700_000_000);
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.duration, "once");
        assert!(rule.enabled);
        assert_eq!(rule.created, 1_700_000_000);
        assert!(!rule.name.is_empty(), "daemon rejects empty rule names");
        assert!(rule.name.contains("allow"));
    }

    #[test]
    fn deny_verdict_produces_deny_rule() {
        let rule = verdict_to_rule(Verdict::Deny, &sample_connection(), 1_700_000_000);
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "once");
        assert!(rule.name.contains("deny"));
    }

    #[test]
    fn rule_name_includes_remote_host_for_traceability() {
        let rule = verdict_to_rule(Verdict::Allow, &sample_connection(), 0);
        assert!(rule.name.contains("github.com"), "got: {}", rule.name);
    }
}
