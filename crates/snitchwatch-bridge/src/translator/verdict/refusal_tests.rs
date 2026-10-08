//! Issue #44, second half: only an absolute `process_path` gets a remembered
//! rule. Everything else (empty, the daemon's `"Kernel connection"`
//! placeholder, a comm / argv[0] fallback) is answered for this connection
//! only, for every scope.

use super::*;

const NOT_BINDABLE: [&str; 4] = ["", "Kernel connection", "curl", "bin/curl"];
const SCOPES: [VerdictScope; 3] = [
    VerdictScope::ThisHost,
    VerdictScope::AnyHostOnDomain,
    VerdictScope::AnyHost,
];
const REMEMBERED: [VerdictDuration; 3] = [
    VerdictDuration::FiveMinutes,
    VerdictDuration::UntilRestart,
    VerdictDuration::Always,
];

fn connection(process_path: &str) -> Connection {
    Connection {
        protocol: "tcp".to_string(),
        dst_ip: "93.184.216.34".to_string(),
        dst_host: "www.example.com".to_string(),
        dst_port: 443,
        process_path: process_path.to_string(),
        ..Default::default()
    }
}

/// Every `process.path` member at any depth of `op`.
fn process_path_members(op: &Operator) -> Vec<&Operator> {
    let mut found: Vec<&Operator> = op.list.iter().flat_map(process_path_members).collect();
    if op.operand == "process.path" {
        found.push(op);
    }
    found
}

#[test]
fn only_an_absolute_path_is_bindable() {
    for path in NOT_BINDABLE {
        assert!(!is_bindable_process_path(path), "{path:?}");
    }
    assert!(is_bindable_process_path("/usr/bin/curl"));
}

#[test]
fn a_remembered_verdict_without_an_absolute_path_is_refused_for_every_scope() {
    for path in NOT_BINDABLE {
        for scope in SCOPES {
            for duration in REMEMBERED {
                for verdict in [Verdict::Allow, Verdict::Deny] {
                    assert_eq!(
                        verdict_to_rule(verdict, duration, scope, &connection(path), 0),
                        Err(RuleRefusal::ProcessFileUnknown),
                        "{path:?} {scope:?} {duration:?} {verdict:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_remembered_verdict_with_an_absolute_path_is_bound_to_it() {
    for scope in SCOPES {
        for duration in REMEMBERED {
            let rule = verdict_to_rule(
                Verdict::Allow,
                duration,
                scope,
                &connection("/usr/bin/curl"),
                0,
            )
            .unwrap_or_else(|e| panic!("{scope:?} {duration:?}: {e:?}"));
            assert_eq!(rule.duration, duration.daemon_duration_str());
            let op = rule.operator.expect("operator");
            let expected_type = match scope {
                VerdictScope::AnyHost => "simple",
                _ => "list",
            };
            assert_eq!(op.r#type, expected_type, "{scope:?}: {op:?}");
            let members = process_path_members(&op);
            assert_eq!(members.len(), 1, "{scope:?}: {op:?}");
            assert_eq!(members[0].data, "/usr/bin/curl");
            assert!(members[0].sensitive, "{scope:?}: case-sensitive match");
            assert!(rule.name.contains("-pcurl-"), "{}", rule.name);
        }
    }
}

#[test]
fn a_once_reply_without_an_absolute_path_is_host_only() {
    for path in NOT_BINDABLE {
        let conn = connection(path);
        for scope in SCOPES {
            let via_verdict =
                verdict_to_rule(Verdict::Allow, VerdictDuration::Once, scope, &conn, 0)
                    .expect("a once reply is never refused");
            assert_eq!(via_verdict, once_rule(Verdict::Allow, scope, &conn, 0));
            assert_eq!(via_verdict.duration, "once");
            let op = via_verdict.operator.expect("operator");
            assert!(
                process_path_members(&op).is_empty(),
                "{path:?} {scope:?}: {op:?}"
            );
            assert!(
                op.operand.starts_with("dest."),
                "{path:?} {scope:?}: {op:?}"
            );
            assert!(
                !via_verdict.name.contains("-p"),
                "{path:?}: a host-only operator gets a host-only name: {}",
                via_verdict.name
            );
            assert_eq!(
                via_verdict.name,
                rule_name_for(Verdict::Allow, "www.example.com", 443, path)
            );
        }
    }
}

#[test]
fn an_any_host_once_deny_without_an_absolute_path_still_reports_the_narrowing() {
    assert_eq!(
        scope_degradation(
            VerdictScope::AnyHost,
            Verdict::Deny,
            &connection("Kernel connection")
        ),
        Some(ScopeDegradation::ProcessPathUnavailable)
    );
}

#[test]
fn the_refusal_reason_is_the_owners_fixed_sentence() {
    assert_eq!(
        RuleRefusal::ProcessFileUnknown.describe(),
        "Snitchwatch couldn't identify this program's file, so this answer applies only to this \
         connection."
    );
}
