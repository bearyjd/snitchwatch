//! Issue #44, second half: only an absolute `process_path` gets a remembered
//! rule. Everything else (empty, the daemon's `"Kernel connection"`
//! placeholder, a comm / argv[0] fallback) is answered for this connection
//! only, for every scope.

use super::*;

/// Not a program file a rule can be bound to: no path at all, the daemon's
/// placeholder, comm / argv[0] fallbacks, and (security review S1) absolute
/// strings that name no real executable or aren't `readlink`'s canonical form.
const NOT_BINDABLE: [&str; 19] = [
    "",
    "Kernel connection",
    "curl",
    "bin/curl",
    "/proc/self/exe",
    "/proc/1234/exe",
    "/proc/1234/fd/3",
    "/memfd:payload",
    "/dev/fd/3",
    "/",
    "/usr//bin/curl",
    "/usr/./bin/curl",
    "/usr/lib/../bin/curl",
    "/usr/bin/..",
    "/usr/bin/.",
    "/usr/bin/",
    "/usr/bin/curl\n",
    "/usr/bin/\u{1b}[31mcurl",
    "/usr/bin/cu\u{7f}rl",
];
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
    for path in [
        "/usr/bin/curl",
        "/usr/lib64/firefox/firefox",
        "/opt/app-1.2/bin/run.sh",
        "/app/bin/.hidden",
        "/home/u/my prog",
    ] {
        assert!(is_bindable_process_path(path), "{path:?}");
    }
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
