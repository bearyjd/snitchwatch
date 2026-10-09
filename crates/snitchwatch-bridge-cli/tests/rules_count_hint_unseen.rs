//! The rules-count hint (issue #65, option c) must not come for rules the
//! daemon holds that the list does not show because of how the two apply a
//! change differently (review of PR #124, plan "Allowance"):
//! - a prompt answered again under the name of a rule the user turned off:
//!   the daemon stores it as `<name>-2` (`setUniqueName`), the bridge
//!   replaces `<name>`;
//! - an add the daemon refused after it stored the rule (`Replace` stores
//!   before `Save`; `scheduleTemporaryRule` fails on a bad duration after).
//!
//! Same bridge and mock daemon as `rules_count_hint.rs`; the daemon's side is
//! `LoaderModel` (`add_prompt_answer` for `Add`, stuck files, durations).

mod count_hint_support;

use count_hint_support::*;
use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::loader::SharedLoader;
use serde_json::Value;
use snitchwatch_bridge::curated::reconcile::EntryStatus;
use snitchwatch_bridge::ws_messages::{
    ClientMessage, RuleCommandOutcome, ServerMessage, VerdictAction, VerdictDuration, VerdictScope,
};
use snitchwatch_bridge_cli::RunningBridge;
use snitchwatch_proto::protocol::{Connection, Rule};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// An authenticated GUI session: with none, the bridge answers every prompt
/// itself. Keep it alive.
async fn gui_session(bridge: &RunningBridge) -> WebSocketStream<UnixStream> {
    let stream = UnixStream::connect(&bridge.ws_socket_path).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    ws.send(Message::Text(bridge.ws_token.as_str().to_string()))
        .await
        .unwrap();
    let ack = tokio::time::timeout(WAIT, ws.next())
        .await
        .expect("no ack")
        .unwrap()
        .unwrap();
    assert!(ack.to_string().contains("authenticated"), "{ack}");
    ws
}

/// The daemon asks about a connection of `curl`, the user answers `Always`
/// or for `duration`, and the rule comes back; the daemon stores it as
/// `Add` does. Returns the rule the bridge answered with and the name the
/// daemon stored it under.
async fn answer_prompt(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
    daemon: &Daemon,
    duration: VerdictDuration,
) -> (Rule, String) {
    let mut asker = daemon.mock.clone();
    let ask = tokio::spawn(async move {
        asker
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "updates.example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
    });
    let row_id = tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::InsertConnectionRows { rows }) = rx.recv().await {
                return rows[0].id.clone();
            }
        }
    })
    .await
    .expect("no prompt");
    bridge
        .inbound_tx
        .send(ClientMessage::SetVerdict {
            row_id,
            verdict: VerdictAction::Allow,
            scope: VerdictScope::ThisHost,
            duration: Some(duration),
            remember: None,
        })
        .await
        .unwrap();
    let rule = tokio::time::timeout(WAIT, ask)
        .await
        .expect("no answer")
        .unwrap()
        .unwrap();
    let stored = daemon
        .model
        .lock()
        .unwrap()
        .add_prompt_answer(rule.clone())
        .unwrap()
        .expect("a remembered answer is stored");
    (rule, stored)
}

/// The wire form of listed rule `name`, from a snapshot answer.
async fn listed(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
    name: &str,
) -> Value {
    drain(rx);
    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::SetRules { rules }) = rx.recv().await {
                return rules
                    .into_iter()
                    .find(|rule| rule["name"] == name)
                    .unwrap_or_else(|| panic!("{name} is not listed"));
            }
        }
    })
    .await
    .expect("no list")
}

/// Turn a listed rule off, as the Rules page does.
async fn turn_off(bridge: &RunningBridge, rx: &mut broadcast::Receiver<ServerMessage>, name: &str) {
    let mut wire = listed(bridge, rx, name).await;
    wire["enabled"] = Value::Bool(false);
    bridge
        .inbound_tx
        .send(ClientMessage::UpdateRule {
            rule_id: name.into(),
            rule: wire,
            request_id: Some("off".into()),
            reply: None,
        })
        .await
        .unwrap();
    assert_eq!(command_result(rx).await, RuleCommandOutcome::Ok);
}

fn names(model: &SharedLoader) -> Vec<String> {
    model.lock().unwrap().memory.keys().cloned().collect()
}

/// H1: the user turns off a remembered rule; the same program asks again
/// (only enabled rules match); the user answers `Always` again.
#[tokio::test]
async fn a_prompt_answered_again_after_the_rule_was_turned_off_never_raises_the_hint() {
    let (_sockets, bridge) = start().await;
    let _gui = gui_session(&bridge).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-other"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;

    let (first, stored) = answer_prompt(&bridge, &mut rx, &daemon, VerdictDuration::Always).await;
    assert_eq!(stored, first.name);
    daemon.ping(8).await;
    turn_off(&bridge, &mut rx, &first.name).await;
    daemon.ping(8).await;
    assert!(!announced(&mut rx).contains(&true));

    let (second, stored) = answer_prompt(&bridge, &mut rx, &daemon, VerdictDuration::Always).await;
    assert_eq!(
        second.name, first.name,
        "the same rule name (rule_name_for)"
    );
    assert_eq!(stored, format!("{}-2", first.name), "setUniqueName");
    assert_eq!(names(&model).len(), 3, "other, the disabled one, and -2");

    daemon.ping(40).await;
    assert!(
        !announced(&mut rx).contains(&true),
        "the daemon holds one rule the list can't show"
    );
    // The list shows the rule once, as the bridge made it.
    let (listed_names, hint) = snapshot(&bridge, &mut rx).await;
    assert_eq!(
        listed_names,
        vec!["100-other".to_string(), first.name.clone()]
    );
    assert!(!hint);

    // The allowance is that one rule: a file the bridge never heard of on top
    // of it is still said.
    file_appears(&model, "100-dropped");
    daemon.ping(2).await;
    assert!(announced(&mut rx).is_empty());
    daemon.ping(1).await;
    assert_eq!(announced(&mut rx), vec![true]);
    bridge.shutdown();
}

/// The variant: "For 5 minutes" over the disabled rule. This is a pause
/// check only: the timed rule stays listed, so the check is held back
/// whatever the allowance says. The pin for the allowance after its expiry
/// is the unit test `a_timed_answer_over_a_disabled_rule_leaves_the_daemon_one_more_after_expiry`
/// (the prune can't be driven end to end: it needs five minutes).
#[tokio::test]
async fn a_five_minute_answer_over_a_disabled_rule_is_held_back_while_it_is_listed() {
    let (_sockets, bridge) = start().await;
    let _gui = gui_session(&bridge).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-other"]);
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;

    let (first, _) = answer_prompt(&bridge, &mut rx, &daemon, VerdictDuration::Always).await;
    turn_off(&bridge, &mut rx, &first.name).await;
    let (second, stored) =
        answer_prompt(&bridge, &mut rx, &daemon, VerdictDuration::FiveMinutes).await;
    assert_eq!(second.duration, "5m");
    assert_eq!(stored, format!("{}-2", first.name));

    daemon.ping(40).await;
    assert!(!announced(&mut rx).contains(&true));
    bridge.shutdown();
}

/// M1: an add the daemon stored and then refused (its rules directory won't
/// take the file). Over TCP the bridge sends no `AddRule` (#35); the
/// recommended rules install with the same `CHANGE_RULE`, which is what the
/// tower r12 setup (an immutable file) refuses after storing the rule.
#[tokio::test]
async fn an_install_the_daemon_refused_after_storing_it_never_raises_the_hint() {
    let (_sockets, _state, bridge) = start_system().await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let model = model(&["100-a"]);
    model.lock().unwrap().stuck.insert(FLATPAK_RULE.into());
    let mut daemon = Daemon::connect(&bridge, 1, &model).await;
    daemon.ping(5).await;

    turn_on(&bridge, &mut rx, "not installed").await;
    assert!(
        names(&model).contains(&FLATPAK_RULE.to_string()),
        "it went in"
    );
    daemon.ping(40).await;
    assert!(!announced(&mut rx).contains(&true));
    let (listed_names, _) = snapshot(&bridge, &mut rx).await;
    assert_eq!(listed_names, vec!["100-a"], "the list can't say");

    // The directory is fixed and the install repeated: now it is listed, and
    // the allowance for it is gone.
    model.lock().unwrap().stuck.clear();
    turn_on(&bridge, &mut rx, "installed").await;
    daemon.ping(40).await;
    assert!(!announced(&mut rx).contains(&true));
    let (listed_names, _) = snapshot(&bridge, &mut rx).await;
    assert!(listed_names.contains(&FLATPAK_RULE.to_string()));
    file_appears(&model, "100-dropped");
    daemon.ping(3).await;
    assert_eq!(announced(&mut rx), vec![true]);
    bridge.shutdown();
}

const FLATPAK: &str = "flatpak-flathub";
const FLATPAK_RULE: &str = "snitchwatch-default-flatpak-flathub";

/// Turn the flatpak entry on and wait until it reads `installed`
/// (`EntryStatus::Installed`) or `not installed` (the refused install).
async fn turn_on(bridge: &RunningBridge, rx: &mut broadcast::Receiver<ServerMessage>, what: &str) {
    let want = if what == "installed" {
        EntryStatus::Installed
    } else {
        EntryStatus::NotInstalled
    };
    bridge
        .inbound_tx
        .send(ClientMessage::SetCuratedDefaults {
            ids: vec![FLATPAK.into()],
            on: true,
        })
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::SetCuratedDefaults { entries, .. }) = rx.recv().await {
                if entries.iter().any(|e| e.id == FLATPAK && e.status == want) {
                    return;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the entry never read {what}"));
}
