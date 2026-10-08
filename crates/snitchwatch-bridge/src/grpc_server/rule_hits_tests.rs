//! Per-rule hit counts through `ping`, the rule snapshot and rule commands
//! (P2.6 Part 1; plan `2026-10-08-rule-insights.md`).

use super::*;
use crate::cache::connections::Verdict;
use crate::ws_messages::VerdictScope;
use snitchwatch_proto::protocol::{Event, NotificationReplyCode, Operator, Statistics};
use tokio::sync::broadcast;

fn service() -> (UiService, broadcast::Receiver<ServerMessage>) {
    let (tx, rx) = broadcast::channel(64);
    let svc = UiService::new(
        Arc::new(Mutex::new(ConnectionCache::new(64))),
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    (svc, rx)
}

fn rule(name: &str) -> Rule {
    Rule {
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: "always".to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn event(rule_name: &str) -> Event {
    Event {
        rule: Some(Rule {
            name: rule_name.to_string(),
            ..Default::default()
        }),
        unixnano: 1_700_000_000_000_000_000,
        ..Default::default()
    }
}

/// A ping with statistics. `rule_hits` is the daemon's global counter,
/// which grows by one for every event it appends (`stats.go`).
async fn ping(svc: &UiService, events: Vec<Event>, uptime: u64, rule_hits: u64) {
    svc.ping(Request::new(PingRequest {
        id: 1,
        stats: Some(Statistics {
            events,
            uptime,
            rule_hits,
            ..Default::default()
        }),
    }))
    .await
    .unwrap();
}

fn hits(svc: &UiService) -> Vec<(String, u64)> {
    match svc.rule_hits_handle().message() {
        ServerMessage::RuleHits { hits, .. } => {
            hits.into_iter().map(|h| (h.name, h.count)).collect()
        }
        other => panic!("expected RuleHits, got {other:?}"),
    }
}

fn lossy(svc: &UiService) -> bool {
    match svc.rule_hits_handle().message() {
        ServerMessage::RuleHits { lossy, .. } => lossy,
        other => panic!("expected RuleHits, got {other:?}"),
    }
}

fn pair(name: &str, count: u64) -> (String, u64) {
    (name.to_string(), count)
}

fn hello() -> NotificationReply {
    NotificationReply {
        id: 0,
        code: NotificationReplyCode::Ok as i32,
        data: String::new(),
    }
}

/// `Subscribe` with these rules, then a `Notifications` stream that says
/// HELLO: the snapshot is committed. Dropping the registration closes the
/// stream and withdraws the list.
async fn connect(svc: &UiService, names: &[&str]) -> Daemon {
    svc.subscribe(Request::new(ClientConfig {
        rules: names.iter().map(|n| rule(n)).collect(),
        ..Default::default()
    }))
    .await
    .unwrap();
    let (stream, outbound) = svc.daemon_commands().open_stream(None);
    svc.daemon_commands().on_reply(stream.id(), &hello());
    Daemon {
        stream,
        _outbound: outbound,
    }
}

/// The daemon's end of a `Notifications` stream; dropping it closes the
/// stream.
struct Daemon {
    stream: crate::daemon_commands::StreamRegistration,
    _outbound: tokio::sync::mpsc::Receiver<Notification>,
}

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_ping_counts_each_events_rule_once_the_list_is_synced() {
    let (svc, _rx) = service();
    let _stream = connect(&svc, &["a", "b"]).await;
    ping(
        &svc,
        vec![event("a"), event("b"), event("a"), event("a")],
        10,
        4,
    )
    .await;
    assert_eq!(hits(&svc), vec![pair("a", 3), pair("b", 1)]);
    svc.ping(Request::new(PingRequest { id: 2, stats: None }))
        .await
        .unwrap();
    assert_eq!(
        hits(&svc),
        vec![pair("a", 3), pair("b", 1)],
        "no stats, no events"
    );
}

#[tokio::test]
async fn events_before_the_first_snapshot_are_adopted_when_it_names_them() {
    let (svc, _rx) = service();
    ping(&svc, vec![event("a"), event("a"), event("gone")], 10, 3).await;
    assert!(hits(&svc).is_empty());
    let _stream = connect(&svc, &["a"]).await;
    assert_eq!(hits(&svc), vec![pair("a", 2)]);
}

#[tokio::test]
async fn a_once_reply_name_is_never_counted() {
    let (svc, _rx) = service();
    let _stream = connect(&svc, &["a"]).await;
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: "example.com".into(),
        dst_port: 443,
        process_path: "/usr/bin/curl".into(),
        ..Default::default()
    };
    let once = crate::translator::verdict::once_rule(
        Verdict::Allow,
        VerdictScope::ThisHost,
        &conn,
        1_800_000_000,
    );
    ping(&svc, vec![event(&once.name), event("a")], 10, 2).await;
    assert_eq!(hits(&svc), vec![pair("a", 1)]);
    // And a later snapshot that doesn't have it can't bring it back.
    drop(_stream);
    let _stream = connect(&svc, &["a"]).await;
    assert_eq!(hits(&svc), vec![pair("a", 1)]);
}

#[tokio::test]
async fn counts_survive_a_daemon_reconnect_and_gap_hits_are_added_on_adoption() {
    let (svc, mut rx) = service();
    let stream = connect(&svc, &["a"]).await;
    ping(&svc, vec![event("a"), event("a"), event("a")], 10, 3).await;

    drop(stream);
    assert!(
        svc.rules_handle().lock().unwrap().is_unknown(),
        "the list was withdrawn"
    );
    assert_eq!(hits(&svc), vec![pair("a", 3)], "withdrawing did not prune");
    while rx.try_recv().is_ok() {}

    ping(&svc, vec![event("a"), event("a")], 11, 5).await;
    let _stream = connect(&svc, &["a"]).await;
    assert_eq!(hits(&svc), vec![pair("a", 5)]);
    assert!(!lossy(&svc), "nothing was lost across the reconnect");
}

#[tokio::test]
async fn a_new_snapshot_without_a_rule_drops_its_count() {
    let (svc, _rx) = service();
    let stream = connect(&svc, &["a", "b"]).await;
    ping(&svc, vec![event("a"), event("b")], 10, 2).await;
    drop(stream);
    let _stream = connect(&svc, &["a"]).await;
    assert_eq!(hits(&svc), vec![pair("a", 1)]);
}

#[tokio::test]
async fn a_confirmed_delete_drops_the_count_and_a_rejected_one_does_not() {
    let (svc, _rx) = service();
    let stream = connect(&svc, &["a", "b"]).await;
    ping(&svc, vec![event("a"), event("b")], 10, 2).await;
    let commands = svc.daemon_commands();

    let rejected = commands.send(delete("a")).unwrap();
    commands.on_reply(
        stream.stream.id(),
        &NotificationReply {
            id: rejected.id(),
            code: NotificationReplyCode::Error as i32,
            data: "no".into(),
        },
    );
    assert_eq!(hits(&svc), vec![pair("a", 1), pair("b", 1)]);

    let confirmed = commands.send(delete("a")).unwrap();
    commands.on_reply(
        stream.stream.id(),
        &NotificationReply {
            id: confirmed.id(),
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        },
    );
    assert_eq!(hits(&svc), vec![pair("b", 1)]);
}

#[tokio::test]
async fn a_confirmed_rule_change_keeps_the_count() {
    let (svc, _rx) = service();
    let stream = connect(&svc, &["a", "b"]).await;
    ping(&svc, vec![event("a"), event("a")], 10, 2).await;
    let commands = svc.daemon_commands();

    let mut disabled = rule("a");
    disabled.enabled = false;
    let change = commands
        .send(Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![disabled],
            ..Default::default()
        })
        .unwrap();
    commands.on_reply(
        stream.stream.id(),
        &NotificationReply {
            id: change.id(),
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        },
    );
    assert_eq!(hits(&svc), vec![pair("a", 2)]);
}

#[tokio::test]
async fn events_the_daemon_counted_but_never_sent_mark_the_counts_incomplete() {
    let (svc, _rx) = service();
    let stream = connect(&svc, &["a"]).await;
    ping(&svc, vec![event("a")], 10, 1).await;
    ping(&svc, vec![event("a"), event("a")], 11, 3).await;
    assert!(!lossy(&svc), "the counter grew by exactly the events");

    // The bridge is away for a while: the daemon's pings fail, and each
    // failed batch is gone (`client.go` `ping` empties it first).
    drop(stream);
    let _stream = connect(&svc, &["a"]).await;
    ping(&svc, vec![event("a")], 40, 9).await;
    assert!(lossy(&svc), "five events never arrived");
    assert_eq!(hits(&svc), vec![pair("a", 4)]);
}

#[tokio::test]
async fn a_daemon_counter_that_goes_down_is_a_restart() {
    // Restarted, then idle for longer than it had run: `uptime` grew.
    let (svc, _rx) = service();
    let _stream = connect(&svc, &["a"]).await;
    ping(&svc, vec![event("a")], 10, 1).await;
    ping(&svc, vec![event("a")], 11, 2).await;
    assert!(!lossy(&svc));
    ping(&svc, vec![event("a")], 5_000, 1).await;
    assert!(lossy(&svc));
    assert_eq!(hits(&svc), vec![pair("a", 3)], "nothing zeroed");
}

#[tokio::test]
async fn a_daemon_restart_marks_the_counts_incomplete_and_keeps_them() {
    let (svc, _rx) = service();
    let _stream = connect(&svc, &["a"]).await;
    ping(&svc, vec![event("a")], 500, 1).await;
    ping(&svc, vec![event("a")], 501, 2).await;
    assert!(!lossy(&svc));
    // The new run's counter grew by exactly its events since the old
    // one's last ping: only `uptime` says.
    ping(&svc, vec![event("a"), event("a"), event("a")], 2, 5).await;
    assert!(lossy(&svc));
    assert_eq!(hits(&svc), vec![pair("a", 5)]);
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A five-minute rule created `created`, named like a remembered verdict.
fn temporary(name: &str, created: i64) -> Rule {
    Rule {
        duration: "5m".to_string(),
        created,
        ..rule(name)
    }
}

/// `Subscribe` with exactly these rules, then HELLO.
async fn connect_with(svc: &UiService, rules: Vec<Rule>) -> Daemon {
    svc.subscribe(Request::new(ClientConfig {
        rules,
        ..Default::default()
    }))
    .await
    .unwrap();
    let (stream, outbound) = svc.daemon_commands().open_stream(None);
    svc.daemon_commands().on_reply(stream.id(), &hello());
    Daemon {
        stream,
        _outbound: outbound,
    }
}

#[tokio::test]
async fn an_expired_temporary_rule_loses_its_count_on_the_wire_and_in_the_file() {
    let (svc, _rx) = service();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rule_hits.json");
    svc.rule_hits_handle().attach_file(file.clone());
    let _stream = connect_with(&svc, vec![temporary("tmp", now_secs() - 301), rule("a")]).await;
    ping(&svc, vec![event("tmp"), event("tmp"), event("a")], 10, 3).await;
    assert_eq!(hits(&svc), vec![pair("a", 1), pair("tmp", 2)]);

    let tick = tokio::spawn(crate::cache::rules::prune_expired_rules_every(
        std::time::Duration::from_secs(30),
        Arc::downgrade(&svc.rules_handle()),
        svc.broadcast.clone(),
        svc.rule_hits_handle(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while hits(&svc).len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the expired rule's count was never dropped");
    tick.abort();
    assert_eq!(hits(&svc), vec![pair("a", 1)]);

    svc.rule_hits_handle().save_now();
    let saved = crate::cache::rule_hits_file::load(&file).unwrap().unwrap();
    let names: Vec<_> = saved.hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, vec!["a"], "and not in the file either");
}

#[tokio::test]
async fn a_rule_re_prompted_before_the_expiry_tick_starts_at_zero() {
    // `rule_name_for` is deterministic, so the new rule has the old name.
    let (svc, _rx) = service();
    let created = now_secs() - 301;
    let _stream = connect_with(&svc, vec![temporary("tmp", created)]).await;
    ping(&svc, vec![event("tmp"), event("tmp")], 10, 2).await;
    assert_eq!(hits(&svc), vec![pair("tmp", 2)]);

    svc.rules.upsert(temporary("tmp", now_secs()));
    assert_eq!(hits(&svc), Vec::new(), "a new rule, not the expired one");
    ping(&svc, vec![event("tmp")], 11, 3).await;
    assert_eq!(hits(&svc), vec![pair("tmp", 1)]);
}

#[tokio::test]
async fn a_remembered_verdict_over_a_live_rule_keeps_its_count() {
    let (svc, _rx) = service();
    let created = now_secs() - 60;
    let _stream = connect_with(&svc, vec![temporary("tmp", created)]).await;
    ping(&svc, vec![event("tmp"), event("tmp")], 10, 2).await;
    svc.rules.upsert(temporary("tmp", now_secs()));
    assert_eq!(hits(&svc), vec![pair("tmp", 2)]);
}
