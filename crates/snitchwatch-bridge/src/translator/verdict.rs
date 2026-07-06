//! Translate a user-supplied `Verdict` (allow / deny) plus its requested
//! [`VerdictDuration`] into the `Rule` proto shape opensnitchd expects as the
//! `AskRule` reply.
//!
//! The M0 spike taught us three things about `Rule`:
//!   - `name` must be non-empty (the daemon rejects empty names).
//!   - `created` is a unix-seconds int64.
//!   - `duration` is a plain string (`Rule.duration`, proto field 8) — see
//!     [`VerdictDuration::daemon_duration_str`] for the exact mapping from the
//!     pending-decision dialog's four duration options to opensnitchd's
//!     `once` / `<Go duration>` / `until restart` / `always` vocabulary.

use crate::cache::connections::Verdict;
use crate::ws_messages::VerdictDuration;
use snitchwatch_proto::protocol::{Connection, Rule};

/// The action token used both in the synthetic rule name and as the `Rule`
/// proto's `action` field.
pub fn verdict_action_str(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
    }
}

/// Build the synthetic once-off rule name the bridge hands back to
/// opensnitchd as the `AskRule` reply for an interactive verdict.
///
/// This is the single source of truth for that name: [`verdict_to_rule`]
/// uses it to build the `Rule` proto, and
/// [`crate::cache::connections::ConnectionCache::resolve`] uses it to
/// populate the resolved row's `matched_rule` for the UI, so the two never
/// drift apart. `host` should already have the empty-dst-host-falls-back-to-
/// dst-ip substitution applied by the caller (both call sites do).
pub fn rule_name_for(verdict: Verdict, host: &str, port: u16) -> String {
    format!("snitchwatch-{}-{host}-{port}", verdict_action_str(verdict))
}

pub fn verdict_to_rule(
    verdict: Verdict,
    duration: VerdictDuration,
    conn: &Connection,
    now_secs: i64,
) -> Rule {
    let action = verdict_action_str(verdict);

    let host = if conn.dst_host.is_empty() {
        conn.dst_ip.as_str()
    } else {
        conn.dst_host.as_str()
    };

    Rule {
        created: now_secs,
        name: rule_name_for(verdict, host, conn.dst_port as u16),
        description: "snitchwatch interactive verdict".to_string(),
        enabled: true,
        precedence: false,
        nolog: false,
        action: action.to_string(),
        duration: duration.daemon_duration_str().to_string(),
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
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            &sample_connection(),
            1_700_000_000,
        );
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.duration, "once");
        assert!(rule.enabled);
        assert_eq!(rule.created, 1_700_000_000);
        assert!(!rule.name.is_empty(), "daemon rejects empty rule names");
        assert!(rule.name.contains("allow"));
    }

    #[test]
    fn deny_verdict_produces_deny_rule() {
        let rule = verdict_to_rule(
            Verdict::Deny,
            VerdictDuration::Once,
            &sample_connection(),
            1_700_000_000,
        );
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "once");
        assert!(rule.name.contains("deny"));
    }

    #[test]
    fn rule_name_includes_remote_host_for_traceability() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            &sample_connection(),
            0,
        );
        assert!(rule.name.contains("github.com"), "got: {}", rule.name);
    }

    #[test]
    fn rule_name_for_matches_verdict_to_rule_name() {
        // Single source of truth: the name the daemon gets back (via
        // `verdict_to_rule`) and the name the UI shows as `matched_rule` (via
        // `rule_name_for`, called from `ConnectionCache::resolve`) must be
        // identical for the same inputs, or the "Show rule" jump would land
        // on a rule that doesn't exist.
        let conn = sample_connection();
        let rule = verdict_to_rule(Verdict::Allow, VerdictDuration::Once, &conn, 1_700_000_000);
        let name = rule_name_for(Verdict::Allow, &conn.dst_host, conn.dst_port as u16);
        assert_eq!(rule.name, name);
    }

    #[test]
    fn each_ui_duration_maps_to_the_expected_daemon_string() {
        let conn = sample_connection();
        let cases = [
            (VerdictDuration::Once, "once"),
            (VerdictDuration::FiveMinutes, "5m"),
            (VerdictDuration::UntilRestart, "until restart"),
            (VerdictDuration::Always, "always"),
        ];
        for (duration, expected) in cases {
            let rule = verdict_to_rule(Verdict::Allow, duration, &conn, 0);
            assert_eq!(rule.duration, expected, "duration mapping for {duration:?}");
        }
    }
}
