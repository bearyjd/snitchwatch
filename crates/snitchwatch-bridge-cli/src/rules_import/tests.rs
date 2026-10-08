//! The apply engine against a real `DaemonCommands`: its per-stream queue,
//! replies and stream close are the real ones.

use super::apply::{self, Applier, Totals};
use super::test_support::*;
use snitchwatch_bridge::rule_io::ImportOutcome;
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Operator, Rule};
use std::time::Duration;

/// Rules absent from the cache at preview time.
async fn run(applier: &Applier, rules: Vec<Rule>) -> Totals {
    let mut totals = Totals::default();
    apply::run(
        applier,
        rules.into_iter().map(|rule| (rule, None)).collect(),
        &mut totals,
    )
    .await;
    totals
}

#[tokio::test]
async fn each_rule_goes_out_alone_as_change_rule_in_name_order() {
    let mut daemon = daemon(vec![host_rule("kept", "deny")]);
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    let mut rx = daemon.broadcast.subscribe();
    let rules = vec![
        host_rule("b", "deny"),
        host_rule("a", "allow"),
        host_rule("c", "deny"),
    ];

    let totals = run(&applier(&daemon), rules).await;

    assert_eq!(
        totals,
        Totals {
            applied: 3,
            ..Default::default()
        }
    );
    assert_eq!(names(&seen), vec!["a", "b", "c"]);
    for n in seen.lock().unwrap().iter() {
        assert_eq!(n.r#type, Action::ChangeRule as i32, "only CHANGE_RULE");
        assert_eq!(n.rules.len(), 1, "one rule per notification");
    }
    let outcomes = progress(&mut rx);
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|(_, o)| *o == ImportOutcome::Applied));
    let cached = daemon.cache.lock().unwrap().rules().unwrap().clone();
    assert!(["a", "b", "c", "kept"]
        .iter()
        .all(|n| cached.contains_key(*n)));
}

/// Daemon text is shown in plain-text labels: hidden characters are
/// stripped and it is shortened, never HTML-escaped (review #15).
#[tokio::test]
async fn a_rejected_rule_reports_the_daemon_text_plainly_and_the_rest_continue() {
    let mut daemon = daemon(Vec::new());
    let long = "x".repeat(300);
    let answer_long = long.clone();
    respond(&mut daemon, move |name| {
        Some(match name {
            "b" => (false, "bad <b>regexp</b>\u{202e}".to_string()),
            "c" => (false, answer_long.clone()),
            _ => (true, String::new()),
        })
    });
    let mut rx = daemon.broadcast.subscribe();
    let rules = vec![
        host_rule("a", "deny"),
        host_rule("b", "deny"),
        host_rule("c", "deny"),
    ];

    let totals = run(&applier(&daemon), rules).await;

    assert_eq!(
        totals,
        Totals {
            applied: 1,
            rejected: 2,
            ..Default::default()
        }
    );
    let outcomes = progress(&mut rx);
    let reason = |name: &str| match &outcomes.iter().find(|(n, _)| n == name).unwrap().1 {
        ImportOutcome::Rejected { reason } => reason.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(reason("b"), "bad <b>regexp</b>");
    assert_eq!(reason("c"), format!("{}…", "x".repeat(200)));
    let cached = daemon.cache.lock().unwrap().rules().unwrap().clone();
    assert!(cached.contains_key("a") && !cached.contains_key("b"));
}

#[tokio::test]
async fn never_more_than_eight_rules_are_in_flight() {
    let mut daemon = daemon(Vec::new());
    let (seen, max_outstanding) = respond(&mut daemon, |_| Some((true, String::new())));
    let rules: Vec<_> = (0..40)
        .map(|i| host_rule(&format!("r{i:02}"), "deny"))
        .collect();

    let totals = run(&applier(&daemon), rules).await;

    assert_eq!(totals.applied, 40);
    assert_eq!(seen.lock().unwrap().len(), 40);
    assert_eq!(*max_outstanding.lock().unwrap(), apply::MAX_IN_FLIGHT);
}

#[tokio::test]
async fn a_full_queue_is_retried_then_reported_busy_and_the_next_rule_goes_out() {
    let mut daemon = daemon(Vec::new());
    // Fill the stream's queue: nobody reads it yet.
    let mut fillers = Vec::new();
    while let Ok(pending) = daemon.commands.send(Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![host_rule("filler", "deny")],
        ..Default::default()
    }) {
        fillers.push(pending);
    }
    drop(fillers);
    let mut rx = daemon.broadcast.subscribe();
    let applier = applier(&daemon);
    let rules = vec![host_rule("a", "deny"), host_rule("b", "deny")];
    let run = tokio::spawn(async move { run(&applier, rules).await });

    // Once "a" is reported busy, drain the queue and answer what follows.
    let busy = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(ServerMessage::RulesImportProgress { name, outcome, .. }) = rx.recv().await {
                return (name, outcome);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(busy.0, "a");
    assert!(matches!(busy.1, ImportOutcome::NotSent { .. }), "{busy:?}");
    while daemon.rx.try_recv().is_ok() {}
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));

    let totals = run.await.unwrap();
    assert_eq!(
        totals,
        Totals {
            applied: 1,
            not_sent: 1,
            ..Default::default()
        }
    );
    assert_eq!(names(&seen), vec!["b"]);
}

#[tokio::test]
async fn a_closed_stream_stops_the_import_and_the_rest_are_not_sent() {
    let mut daemon = daemon(Vec::new());
    let mut rx = daemon.broadcast.subscribe();
    let applier = applier(&daemon);
    let rules: Vec<_> = (0..12)
        .map(|i| host_rule(&format!("r{i:02}"), "deny"))
        .collect();
    let run = tokio::spawn(async move { run(&applier, rules).await });

    // The first notification arrives; then the daemon's stream goes away.
    tokio::time::timeout(Duration::from_secs(5), daemon.rx.recv())
        .await
        .unwrap()
        .unwrap();
    drop(daemon.registration.take());

    let totals = run.await.unwrap();
    assert_eq!(totals.applied, 0);
    assert_eq!(totals.no_answer, apply::MAX_IN_FLIGHT as u32, "{totals:?}");
    assert_eq!(
        totals.not_sent,
        12 - apply::MAX_IN_FLIGHT as u32,
        "{totals:?}"
    );
    assert_eq!(progress(&mut rx).len(), 12, "one outcome per rule");
}

#[tokio::test]
async fn without_a_daemon_nothing_is_sent() {
    let mut daemon = daemon(Vec::new());
    drop(daemon.registration.take());
    let totals = run(
        &applier(&daemon),
        vec![host_rule("a", "deny"), host_rule("b", "deny")],
    )
    .await;
    assert_eq!(
        totals,
        Totals {
            not_sent: 2,
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn an_unanswered_rule_is_reported_as_no_answer() {
    let mut daemon = daemon(Vec::new());
    respond(&mut daemon, |name| {
        (name != "a").then(|| (true, String::new()))
    });
    let applier = applier(&daemon).with_reply_timeout(Duration::from_millis(100));
    let totals = run(
        &applier,
        vec![host_rule("a", "deny"), host_rule("b", "deny")],
    )
    .await;
    assert_eq!(
        totals,
        Totals {
            applied: 1,
            no_answer: 1,
            ..Default::default()
        }
    );
}

/// Review L1: a daemon that stops answering ends the import after twenty
/// unanswered rules in a row, rather than waiting on every rule.
#[tokio::test]
async fn twenty_unanswered_rules_in_a_row_stop_the_import() {
    let mut daemon = daemon(Vec::new());
    respond(&mut daemon, |_| None);
    let applier = applier(&daemon).with_reply_timeout(Duration::from_millis(20));
    let rules: Vec<_> = (0..40)
        .map(|i| host_rule(&format!("r{i:02}"), "deny"))
        .collect();
    let totals = run(&applier, rules).await;
    // Twenty in a row stop it; what was already in flight still reports.
    let most = apply::MAX_UNANSWERED_IN_A_ROW + apply::MAX_IN_FLIGHT as u32;
    assert!(
        (apply::MAX_UNANSWERED_IN_A_ROW..most).contains(&totals.no_answer),
        "{totals:?}"
    );
    assert_eq!(totals.not_sent, 40 - totals.no_answer, "{totals:?}");
}

/// Review M2: just before each send, the rule is checked against what the
/// preview compared; one changed since is not sent.
#[tokio::test]
async fn a_rule_changed_since_the_preview_is_not_sent() {
    let previewed = host_rule("a", "deny");
    let mut daemon = daemon(vec![previewed.clone()]);
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    let mut rx = daemon.broadcast.subscribe();
    let mut toggled = previewed.clone();
    toggled.enabled = false;
    daemon.cache.lock().unwrap().upsert(toggled);

    let mut totals = Totals::default();
    apply::run(
        &applier(&daemon),
        vec![
            (host_rule("a", "allow"), Some(previewed)),
            (host_rule("b", "deny"), None),
        ],
        &mut totals,
    )
    .await;

    assert_eq!(names(&seen), vec!["b"]);
    assert_eq!((totals.applied, totals.not_sent), (1, 1));
    let outcomes = progress(&mut rx);
    assert!(matches!(
        &outcomes.iter().find(|(n, _)| n == "a").unwrap().1,
        ImportOutcome::NotSent { reason } if reason == apply::CHANGED_SINCE_PREVIEW
    ));
}

/// A new daemon stream (a reconnect) ends the import: what was previewed
/// was compared with the old one's rules.
#[tokio::test]
async fn a_daemon_reconnect_stops_the_import() {
    let mut daemon = daemon(Vec::new());
    let applier = applier(&daemon).with_reply_timeout(Duration::from_millis(100));
    // The new stream brings its own (empty) snapshot, so the rules still
    // match the preview: only the reconnect itself stops the import.
    daemon.sync.stage(None, Vec::new());
    let (stream, _rx) = daemon.commands.open_stream(None);
    daemon.commands.on_reply(stream.id(), &reply(0, true, ""));
    assert!(!daemon.cache.lock().unwrap().is_unknown());
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));

    let totals = run(
        &applier,
        vec![host_rule("a", "deny"), host_rule("b", "deny")],
    )
    .await;

    assert!(names(&seen).is_empty());
    assert_eq!(totals.not_sent, 2, "{totals:?}");
}

#[tokio::test]
async fn a_rule_the_policy_refuses_at_apply_time_is_not_sent() {
    let mut daemon = daemon(Vec::new());
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    let mut lists = host_rule("a", "deny");
    lists.operator = Some(Operator {
        r#type: "lists".into(),
        operand: "lists.domains".into(),
        data: "/etc".into(),
        ..Default::default()
    });
    let reserved = host_rule("z00-blocklist:x:domains", "allow");
    let curated = host_rule("snitchwatch-default-x", "allow");

    let totals = run(
        &applier(&daemon),
        vec![lists, reserved, curated, host_rule("ok", "deny")],
    )
    .await;

    assert_eq!(
        totals,
        Totals {
            applied: 1,
            rejected: 3,
            ..Default::default()
        }
    );
    assert_eq!(names(&seen), vec!["ok"]);
}

/// A name a rule command is changing (an add, a rename) isn't sent by an
/// import meanwhile (P2.1 re-review M5), and an import's own rule in flight
/// keeps its name busy until its reply.
#[tokio::test]
async fn a_name_another_change_is_saving_is_not_sent() {
    let mut daemon = daemon(Vec::new());
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    let busy = crate::busy::BusyNames::default();
    let held = busy.claim(&["b"]).unwrap();
    let applier = Applier::new(
        daemon.commands.clone(),
        daemon.cache.clone(),
        crate::replier::Replier::broadcast(daemon.broadcast.clone()),
        "p".into(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        busy.clone(),
    );
    let mut rx = daemon.broadcast.subscribe();
    let totals = run(
        &applier,
        vec![host_rule("a", "deny"), host_rule("b", "deny")],
    )
    .await;
    assert_eq!(totals.applied, 1);
    assert_eq!(totals.not_sent, 1);
    assert_eq!(names(&seen), vec!["a"]);
    let outcomes = progress(&mut rx);
    assert!(outcomes.iter().any(|(name, outcome)| name == "b"
        && *outcome
            == ImportOutcome::NotSent {
                reason: apply::BEING_CHANGED.to_string()
            }));
    drop(held);
    assert!(
        busy.claim(&["a", "b"]).is_some(),
        "the import let its names go"
    );
}
