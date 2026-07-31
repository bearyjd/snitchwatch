//! Translate a user-supplied `Verdict` (allow / deny) plus its requested
//! [`VerdictDuration`] and [`VerdictScope`] into the `Rule` proto shape
//! opensnitchd expects as the `AskRule` reply.
//!
//! The M0 spike taught us three things about `Rule`:
//!   - `name` must be non-empty (the daemon rejects empty names).
//!   - `created` is a unix-seconds int64.
//!   - `duration` is a plain string (`Rule.duration`, proto field 8) — see
//!     [`VerdictDuration::daemon_duration_str`] for the exact mapping from the
//!     pending-decision dialog's four duration options to opensnitchd's
//!     `once` / `<Go duration>` / `until restart` / `always` vocabulary.
//!
//! A fourth thing, discovered by issue #14: `operator` is **not** optional in
//! practice. `vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize` hard-
//! rejects any `Rule` with `Operator == nil` ("invalid operator"), which
//! makes `ui/client.go`'s `Ask()` return `nil` and the daemon fall through to
//! `DefaultAction` — every verdict this bridge ever sent was silently
//! discarded before this fix. [`build_operator`] is the single place that
//! turns a [`VerdictScope`] into a real `Operator`, using the daemon's own
//! type/operand vocabulary (`vendor/opensnitch/daemon/rule/operator.go`):
//! `Type` is `"simple"`/`"regexp"`, `Operand` is `"dest.host"`/`"dest.ip"`/
//! `"process.path"`.

use crate::cache::connections::Verdict;
use crate::ws_messages::{VerdictDuration, VerdictScope};
use snitchwatch_proto::protocol::{Connection, Operator, Rule};
use std::net::IpAddr;

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
    scope: VerdictScope,
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
        operator: Some(build_operator(scope, conn)),
    }
}

fn simple_operator(operand: &str, data: &str) -> Operator {
    Operator {
        r#type: "simple".to_string(),
        operand: operand.to_string(),
        data: data.to_string(),
        sensitive: false,
        list: Vec::new(),
    }
}

/// `ThisHost` scope: match the exact destination host, falling back to the
/// destination IP when the connection carries no resolved hostname (a bare-
/// IP connection has an empty `dst_host`).
fn this_host_operator(conn: &Connection) -> Operator {
    if conn.dst_host.is_empty() {
        simple_operator("dest.ip", conn.dst_ip.as_str())
    } else {
        simple_operator("dest.host", conn.dst_host.as_str())
    }
}

/// `AnyHostOnDomain` scope: wildcard the leftmost label of the destination
/// host, matching any subdomain of the parent domain (but not the bare
/// parent domain itself, nor the exact original host again).
///
/// Degrades to [`this_host_operator`] whenever the wildcard can't be built
/// safely: no hostname at all, a bare IP (no labels to drop), or a
/// single-label host (dropping its only label would leave nothing to anchor
/// the pattern on). A too-broad allow rule is a security defect, so this
/// function never emits a pattern that isn't scoped to a real parent domain.
fn any_host_on_domain_operator(conn: &Connection) -> Operator {
    let host = conn.dst_host.as_str();
    if host.is_empty() || host.parse::<IpAddr>().is_ok() {
        return this_host_operator(conn);
    }

    // Require at least 3 labels (e.g. `www.example.com`), not just >1:
    // a 2-label host (`example.com`) would otherwise wildcard down to a
    // bare TLD-ish remainder (`^.*\.com$`), which is functionally "matches
    // everything under .com" — the exact too-broad-allow-rule defect this
    // scope must never produce.
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 3 {
        return this_host_operator(conn);
    }

    let parent_domain = labels[1..].join(".");
    let pattern = format!("^.*\\.{}$", regex::escape(&parent_domain));
    Operator {
        r#type: "regexp".to_string(),
        operand: "dest.host".to_string(),
        data: pattern,
        sensitive: false,
        list: Vec::new(),
    }
}

/// `AnyHost` scope: the daemon has no "match everything" operator (`operator`
/// is mandatory), so this scopes the rule to the process instead — Little
/// Snitch's "allow this app to connect anywhere" semantics. Degrades to
/// [`this_host_operator`] when the process path is unknown, rather than risk
/// an operator that matches every connection from every process.
fn any_host_operator(conn: &Connection) -> Operator {
    if conn.process_path.is_empty() {
        tracing::warn!(
            dst_host = %conn.dst_host,
            dst_ip = %conn.dst_ip,
            "AnyHost verdict scope requested with an empty process_path; \
             degrading to ThisHost rather than emit an unscoped rule"
        );
        return this_host_operator(conn);
    }
    simple_operator("process.path", conn.process_path.as_str())
}

fn build_operator(scope: VerdictScope, conn: &Connection) -> Operator {
    match scope {
        VerdictScope::ThisHost => this_host_operator(conn),
        VerdictScope::AnyHostOnDomain => any_host_on_domain_operator(conn),
        VerdictScope::AnyHost => any_host_operator(conn),
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
            VerdictScope::ThisHost,
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
            VerdictScope::ThisHost,
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
            VerdictScope::ThisHost,
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
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &conn,
            1_700_000_000,
        );
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
            let rule = verdict_to_rule(Verdict::Allow, duration, VerdictScope::ThisHost, &conn, 0);
            assert_eq!(rule.duration, expected, "duration mapping for {duration:?}");
        }
    }

    // -- operator population (issue #14) --------------------------------

    #[test]
    fn every_rule_carries_a_non_none_operator() {
        // The daemon hard-rejects `operator: None` (rule.go's Deserialize),
        // so this must never regress for any scope.
        for scope in [
            VerdictScope::ThisHost,
            VerdictScope::AnyHostOnDomain,
            VerdictScope::AnyHost,
        ] {
            let rule = verdict_to_rule(
                Verdict::Allow,
                VerdictDuration::Once,
                scope,
                &sample_connection(),
                0,
            );
            assert!(
                rule.operator.is_some(),
                "scope {scope:?} produced a None operator"
            );
        }
    }

    #[test]
    fn this_host_scope_matches_on_dest_host() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &sample_connection(),
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "github.com");
    }

    #[test]
    fn this_host_scope_falls_back_to_dest_ip_when_host_is_empty() {
        let mut conn = sample_connection();
        conn.dst_host = String::new();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.ip");
        assert_eq!(op.data, "140.82.121.4");
    }

    #[test]
    fn any_host_on_domain_scope_wildcards_the_leftmost_label() {
        let mut conn = sample_connection();
        conn.dst_host = "www.example.com".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "regexp");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, r"^.*\.example\.com$");
    }

    #[test]
    fn any_host_on_domain_scope_escapes_regex_metacharacters_in_the_domain() {
        let mut conn = sample_connection();
        conn.dst_host = "a.b+c.example.com".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        // The literal domain must be escaped, not interpolated raw — `+`
        // is a regex quantifier and must not survive unescaped.
        assert!(
            !op.data.contains("b+c"),
            "unescaped metachar leaked: {}",
            op.data
        );
        assert_eq!(op.data, r"^.*\.b\+c\.example\.com$");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_to_this_host_for_single_label_host() {
        let mut conn = sample_connection();
        conn.dst_host = "localhost".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        // Never a bare wildcard-everything pattern.
        assert_ne!(
            op.r#type, "regexp",
            "must not emit a regexp for a single-label host"
        );
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "localhost");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_to_this_host_for_two_label_host() {
        // `example.com` has only two labels: wildcarding the leftmost would
        // leave a bare TLD-ish remainder (`^.*\.com$`), which matches
        // "everything under .com" — a too-broad allow rule. Must degrade.
        let conn = sample_connection(); // dst_host = "github.com"
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "github.com");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_to_this_host_for_bare_ip() {
        let mut conn = sample_connection();
        conn.dst_host = "203.0.113.5".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_ne!(
            op.r#type, "regexp",
            "must not emit a regexp for a bare IP host"
        );
    }

    #[test]
    fn any_host_scope_matches_on_process_path() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHost,
            &sample_connection(),
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "process.path");
        assert_eq!(op.data, "/usr/bin/curl");
    }

    #[test]
    fn any_host_scope_degrades_to_this_host_when_process_path_is_empty() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHost,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        // Never an operator matching every process (which is what an empty
        // process.path data would functionally amount to).
        assert_ne!(op.operand, "process.path");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "github.com");
    }
}
