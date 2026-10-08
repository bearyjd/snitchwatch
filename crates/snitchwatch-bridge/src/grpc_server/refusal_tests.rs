//! Issue #44, second half: `ask_rule` answers a remembered verdict it can't
//! bind to an absolute executable path once, keeps it out of the rules cache
//! and `UpdateRules`, and tells every client why. Also the controls: what it
//! does remember for an absolute path, including the GUI's inline Deny.

use super::*;
use crate::cache::rules::RulesCache;
use crate::notice::Notice;
use crate::translator::connection::ask_row_id;
use crate::translator::process_binding::RuleRefusal;
use crate::ws_messages::{VerdictDuration, VerdictScope};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};

struct Asked {
    rule: Rule,
    /// Everything broadcast after the `InsertConnectionRows`, in order.
    messages: Vec<ServerMessage>,
    notices: Vec<Notice>,
    rules: RulesCache,
}

fn kernel_connection(process_path: &str) -> Connection {
    Connection {
        protocol: "tcp".into(),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: process_path.into(),
        ..Default::default()
    }
}

/// Ask once with an authenticated GUI, resolve the row as given, and collect
/// what the bridge sent. Every send in `ask_rule` happens before its reply,
/// so draining with `try_recv` afterwards sees all of them.
async fn ask_and_resolve(
    conn: Connection,
    verdict: Verdict,
    duration: VerdictDuration,
    scope: VerdictScope,
) -> Asked {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(64);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let mut notice_rx = notice_bus.subscribe();
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(crate::tray_state::TrayStatePublisher::new()),
        notice_bus,
        Arc::new(FilterPause::new()),
    );
    // A synced (empty) daemon list, so an upsert would be visible.
    svc.rules_handle().lock().unwrap().replace_all(Vec::new());
    let _gui_session = svc.client_presence().authenticated_session();

    let asking = svc.clone();
    let ask = tokio::spawn(async move { asking.ask_rule(Request::new(conn)).await });
    match tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("ask_rule did not broadcast")
        .expect("broadcast error")
    {
        ServerMessage::InsertConnectionRows { rows } => assert_eq!(rows[0].id, ask_row_id(1)),
        other => panic!("expected InsertConnectionRows, got {other:?}"),
    }
    cache
        .lock()
        .await
        .resolve(&ask_row_id(1), verdict, duration, scope)
        .unwrap();
    let rule = tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .expect("ask_rule did not reply")
        .unwrap()
        .expect("ask_rule failed")
        .into_inner();

    let mut messages = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        messages.push(msg);
    }
    let mut notices = Vec::new();
    while let Ok(notice) = notice_rx.try_recv() {
        notices.push(notice);
    }
    let rules = svc.rules_handle().lock().unwrap().clone();
    Asked {
        rule,
        messages,
        notices,
        rules,
    }
}

fn not_remembered(asked: &Asked) -> Vec<(&str, &str)> {
    asked
        .messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::VerdictNotRemembered { row_id, reason } => {
                Some((row_id.as_str(), reason.as_str()))
            }
            _ => None,
        })
        .collect()
}

fn update_rules_count(asked: &Asked) -> usize {
    asked
        .messages
        .iter()
        .filter(|m| matches!(m, ServerMessage::UpdateRules { .. }))
        .count()
}

#[tokio::test]
async fn a_remembered_allow_for_a_kernel_connection_is_answered_once_and_explained() {
    let asked = ask_and_resolve(
        kernel_connection("Kernel connection"),
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
    )
    .await;

    assert_eq!(asked.rule.action, "allow");
    assert_eq!(asked.rule.duration, "once");
    let op = asked.rule.operator.as_ref().expect("operator");
    assert_eq!(
        (op.operand.as_str(), op.data.as_str()),
        ("dest.host", "example.com")
    );
    assert_eq!(update_rules_count(&asked), 0, "{:?}", asked.messages);
    assert_eq!(
        not_remembered(&asked),
        vec![(
            ask_row_id(1).as_str(),
            RuleRefusal::ProcessFileUnknown.describe()
        )]
    );
    let notices: Vec<_> = asked
        .notices
        .iter()
        .filter(|n| matches!(n, Notice::VerdictNotRemembered { .. }))
        .collect();
    assert_eq!(notices, vec![&Notice::VerdictNotRemembered { row_id: 1 }]);
    assert_eq!(
        asked.rules,
        RulesCache::Synced(Default::default()),
        "a rule the daemon never stores must not enter the rules cache"
    );
}

/// The `AnyHost` scope used to fall back to a remembered host-only rule.
#[tokio::test]
async fn a_remembered_any_host_deny_without_a_path_is_answered_once() {
    let asked = ask_and_resolve(
        kernel_connection(""),
        Verdict::Deny,
        VerdictDuration::UntilRestart,
        VerdictScope::AnyHost,
    )
    .await;

    assert_eq!(asked.rule.action, "deny");
    assert_eq!(asked.rule.duration, "once");
    let op = asked.rule.operator.as_ref().expect("operator");
    assert_eq!(
        op.operand, "dest.host",
        "never an unscoped process.path rule"
    );
    assert_eq!(update_rules_count(&asked), 0, "{:?}", asked.messages);
    assert_eq!(not_remembered(&asked).len(), 1, "{:?}", asked.messages);
    assert!(
        asked
            .messages
            .iter()
            .any(|m| matches!(m, ServerMessage::DenyScopeNarrowed { .. })),
        "the once-only deny still covers this host only: {:?}",
        asked.messages
    );
    assert_eq!(asked.rules, RulesCache::Synced(Default::default()));
}

/// Control: an absolute path is still remembered, cached and announced.
#[tokio::test]
async fn a_remembered_allow_with_an_absolute_path_is_still_remembered() {
    let asked = ask_and_resolve(
        kernel_connection("/usr/bin/curl"),
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
    )
    .await;

    assert_eq!(asked.rule.duration, "always");
    assert_eq!(update_rules_count(&asked), 1, "{:?}", asked.messages);
    assert!(not_remembered(&asked).is_empty(), "{:?}", asked.messages);
    assert!(!asked
        .notices
        .iter()
        .any(|n| matches!(n, Notice::VerdictNotRemembered { .. })));
    let RulesCache::Synced(rules) = &asked.rules else {
        panic!("cache unsynced")
    };
    assert!(rules.contains_key(&asked.rule.name), "{rules:?}");
}

/// The Kirigami shell's inline Deny for a program with an absolute path asks
/// for `{deny, ThisHost, UntilRestart}` (plan
/// `2026-10-08-inline-deny-until-restart.md`). Pins the reply that makes it
/// work: a rule the daemon stores until it restarts, bound to the program and
/// the destination, cached and announced once. A `verdict.rs` refactor must
/// not quietly turn it back into a once-only or all-apps deny.
async fn assert_inline_deny_is_app_bound_until_restart(dst_host: &str, destination: (&str, &str)) {
    let asked = ask_and_resolve(
        Connection {
            protocol: "tcp".into(),
            dst_host: dst_host.into(),
            dst_ip: "140.82.112.3".into(),
            dst_port: 443,
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        },
        Verdict::Deny,
        VerdictDuration::UntilRestart,
        VerdictScope::ThisHost,
    )
    .await;

    assert_eq!(
        (asked.rule.action.as_str(), asked.rule.duration.as_str()),
        ("deny", "until restart")
    );
    let op = asked.rule.operator.as_ref().expect("operator");
    assert_eq!(op.r#type, "list", "{op:?}");
    assert_eq!(op.list.len(), 2, "{op:?}");
    let (process, host) = (&op.list[0], &op.list[1]);
    assert_eq!(
        (process.operand.as_str(), process.data.as_str()),
        ("process.path", "/usr/bin/curl")
    );
    assert!(process.sensitive, "a path matches case-sensitively");
    assert_eq!((host.operand.as_str(), host.data.as_str()), destination);
    assert_eq!(update_rules_count(&asked), 1, "{:?}", asked.messages);
    assert!(not_remembered(&asked).is_empty(), "{:?}", asked.messages);
    let RulesCache::Synced(rules) = &asked.rules else {
        panic!("cache unsynced")
    };
    assert!(rules.contains_key(&asked.rule.name), "{rules:?}");
}

#[tokio::test]
async fn inline_deny_until_restart_reply_is_app_bound_and_remembered() {
    assert_inline_deny_is_app_bound_until_restart("github.com", ("dest.host", "github.com")).await;
}

#[tokio::test]
async fn inline_deny_until_restart_without_a_host_binds_the_ip() {
    assert_inline_deny_is_app_bound_until_restart("", ("dest.ip", "140.82.112.3")).await;
}

#[tokio::test]
async fn a_paused_ask_with_an_empty_path_still_auto_allows_once() {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let filter_pause = Arc::new(FilterPause::new());
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    let svc = UiService::new(
        cache,
        tx,
        Arc::new(crate::tray_state::TrayStatePublisher::new()),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause,
    );
    let _gui_session = svc.client_presence().authenticated_session();

    let rule = svc
        .ask_rule(Request::new(kernel_connection("")))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (rule.action.as_str(), rule.duration.as_str()),
        ("allow", "once")
    );
    while let Ok(msg) = rx.try_recv() {
        assert!(
            !matches!(msg, ServerMessage::VerdictNotRemembered { .. }),
            "nothing was asked to be remembered"
        );
    }
}

#[test]
fn verdict_not_remembered_round_trips_via_json() {
    let msg = ServerMessage::VerdictNotRemembered {
        row_id: "ask-7".to_string(),
        reason: RuleRefusal::ProcessFileUnknown.describe().to_string(),
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["action"], "verdictNotRemembered");
    assert_eq!(json["rowId"], "ask-7");
    assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), msg);

    let notice = ServerMessage::Notice {
        notice: Notice::VerdictNotRemembered { row_id: 7 },
    };
    let json = serde_json::to_string(&notice).unwrap();
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&json).unwrap(),
        notice
    );
}

/// #44 security review S3: the pending notice's process path reaches every
/// desktop notifier body, and freedesktop servers render markup there.
#[tokio::test]
async fn the_pending_notice_carries_a_display_safe_process_path() {
    let asked = ask_and_resolve(
        kernel_connection("/tmp/<b>evil</b>\x1b[31m\u{202e}"),
        Verdict::Allow,
        VerdictDuration::Once,
        VerdictScope::ThisHost,
    )
    .await;
    let process = asked
        .notices
        .iter()
        .find_map(|n| match n {
            Notice::Pending { process, .. } => Some(process.clone()),
            _ => None,
        })
        .expect("a pending notice");
    assert_eq!(process, "/tmp/&lt;b&gt;evil&lt;/b&gt;[31m");
}
