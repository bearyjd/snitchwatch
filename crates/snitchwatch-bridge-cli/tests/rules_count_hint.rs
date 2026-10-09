//! The Rules page hint when the daemon's rule count disagrees (issue #65,
//! option c; plan `docs/superpowers/plans/2026-10-09-rules-count-hint-65.md`),
//! end to end: a bridge, and a mock daemon that answers commands from a model
//! of its loader and reports that loader's `NumRules()` in every ping.
//!
//! The numbers asserted are the plan's: two readings are ignored after the
//! bridge changes its list, three repeats of one disagreement raise the hint
//! and three agreeing readings clear it. What the daemon may hold that the
//! list doesn't show is in `rules_count_hint_unseen.rs`.

mod count_hint_support;

use count_hint_support::*;
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};
use snitchwatch_proto::protocol::{ClientConfig, NotificationReply, NotificationReplyCode};

/// The #65 case: the daemon's watcher loads a rule file and nothing reaches
/// the bridge. The hint appears after three pings, once; it does not touch
/// the list; it goes after three agreeing pings, once.
#[tokio::test]
async fn a_rule_file_that_appears_raises_the_hint_once_and_removing_it_clears_it() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;

    daemon.ping(15).await;
    assert!(!announced(&mut rx).contains(&true), "agreement is silent");

    file_appears(&model, "100-dropped");
    daemon.ping(2).await;
    assert!(drain(&mut rx).is_empty(), "two readings are not enough");
    daemon.ping(1).await;
    let seen = drain(&mut rx);
    assert!(
        matches!(
            seen.as_slice(),
            [ServerMessage::RulesNotShown {
                count_mismatch: true,
                listed: true,
                ..
            }]
        ),
        "{seen:?}"
    );

    daemon.ping(40).await;
    assert!(
        drain(&mut rx).is_empty(),
        "it holds without a word: no list re-sent, no flicker"
    );

    // Advice only: the list is what it was, and a GUI that comes late is told.
    let (names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(names, vec!["100-a", "100-b"]);
    assert!(hint);

    file_goes(&model, "100-dropped");
    daemon.ping(2).await;
    assert!(
        drain(&mut rx).is_empty(),
        "two agreeing readings are not enough"
    );
    daemon.ping(1).await;
    assert_eq!(announced(&mut rx), vec![false]);
    daemon.ping(40).await;
    assert!(drain(&mut rx).is_empty());
}

/// A reading built before the daemon applied (or the bridge learned of) the
/// bridge's own change is old: it never raises the hint.
#[tokio::test]
async fn a_change_the_bridge_made_never_raises_the_hint_even_with_old_readings() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b", "100-c", "100-d", "100-e"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;
    drain(&mut rx);

    for (i, name) in ["100-a", "100-b", "100-c"].into_iter().enumerate() {
        bridge
            .inbound_tx
            .send(delete_rule(name, &format!("del-{i}")))
            .await
            .unwrap();
        assert_eq!(command_result(&mut rx).await, RuleCommandOutcome::Ok);
        let before = model.lock().unwrap().num_rules() + 1;
        // Pings built just before the daemon applied it.
        daemon.report(before).await;
        daemon.report(before).await;
        daemon.report(before).await;
        daemon.ping(8).await;
    }
    assert!(!announced(&mut rx).contains(&true));
    let (names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(names, vec!["100-d", "100-e"]);
    assert!(!hint);
}

/// The daemon has applied a command whose `OK` the bridge has not seen: the
/// counts differ for as long as the reply takes, and that is not a hint.
#[tokio::test]
async fn a_command_still_waiting_for_its_reply_never_raises_the_hint() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b"]);
    let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    let rules = model.lock().unwrap().snapshot();
    mock.subscribe_with_config(ClientConfig {
        name: "mock".into(),
        rules,
        ..Default::default()
    })
    .await
    .unwrap();
    let (replies, mut inbound) = mock.open_notifications().await.unwrap();
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    let mut pings = 0;
    for _ in 0..5 {
        pings += 1;
        mock.ping_reporting_rules(pings, 2, pings).await.unwrap();
    }

    bridge
        .inbound_tx
        .send(delete_rule("100-b", "slow"))
        .await
        .unwrap();
    let command = tokio::time::timeout(WAIT, inbound.recv())
        .await
        .expect("no command")
        .expect("stream closed");
    model.lock().unwrap().apply(&command).unwrap();
    // Applied, not yet answered: one is the daemon's count, two the list's.
    for _ in 0..12 {
        pings += 1;
        mock.ping_reporting_rules(pings, 1, pings).await.unwrap();
    }
    assert!(!announced(&mut rx).contains(&true));

    replies
        .send(NotificationReply {
            id: command.id,
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(command_result(&mut rx).await, RuleCommandOutcome::Ok);
    for _ in 0..12 {
        pings += 1;
        mock.ping_reporting_rules(pings, 1, pings).await.unwrap();
    }
    assert!(!announced(&mut rx).contains(&true));

    // The pause is not permanent: a file the bridge never heard of still counts.
    file_appears(&model, "100-dropped");
    for _ in 0..3 {
        pings += 1;
        mock.ping_reporting_rules(pings, 2, pings).await.unwrap();
    }
    assert_eq!(announced(&mut rx), vec![true]);
}

/// A daemon that reconnects sends its list again: that adoption is the fix
/// the hint points at, and the hint goes with it, without coming back.
#[tokio::test]
async fn a_reconnected_daemons_list_replaces_the_hint() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a", "100-b"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;
    drain(&mut rx);
    file_appears(&model, "100-dropped");
    daemon.ping(3).await;
    assert_eq!(announced(&mut rx), vec![true]);

    drop(daemon);
    let mut daemon = Daemon::connect(&bridge, 2, &model).await;
    let (names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(names, vec!["100-a", "100-b", "100-dropped"]);
    assert!(!hint, "the new list is the new baseline");
    daemon.ping(40).await;
    assert!(
        !announced(&mut rx).contains(&true),
        "and the counts agree now"
    );
}

/// The daemon sends no ping at all without new events, and a ping without
/// statistics says nothing about rules.
#[tokio::test]
async fn pings_without_statistics_say_nothing() {
    let (_sockets, bridge) = start().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    file_appears(&model, "100-dropped");
    for id in 1..=30 {
        daemon.mock.ping(id).await.unwrap();
    }
    assert!(!announced(&mut rx).contains(&true));
    // A zero is not evidence either: proto3 cannot say "not reported".
    for _ in 0..30 {
        daemon.report(0).await;
    }
    assert!(!announced(&mut rx).contains(&true));
}
