//! Profile enforcement end to end (issue #46 Part 2): a bridge with a saved
//! state directory and the mock daemon. Activating a profile installs its
//! rules, a stray rule the bridge made earlier is deleted after the
//! daemon's snapshot, a restart finds the rules in place without resending
//! them, and a per-user bridge installs nothing and says why.

use std::path::Path;
use std::time::Duration;

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::rule_policy::PROFILE_MANAGED_REASON;
use snitchwatch_bridge::ws_messages::{
    ClientMessage, ProfileRuleWire, ProfileSummary, RuleCommandOutcome, ServerMessage,
    ENFORCEMENT_RULE_INSTALLED,
};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{Action, ClientConfig, Notification, Operator, Rule};
use tokio::sync::{broadcast, mpsc};

const WAIT: Duration = Duration::from_secs(10);
const RULE_NAME: &str = "850-profile:home:0000-r1";

struct Bridge {
    _sockets: tempfile::TempDir,
    bridge: RunningBridge,
    rx: broadcast::Receiver<ServerMessage>,
}

async fn start_on(state: &Path, mode: BridgeMode) -> Bridge {
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.canonicalize().unwrap()),
            blocklist_fetcher: None,
            mode,
        },
    )
    .await
    .unwrap();
    let rx = bridge.broadcast_tx.subscribe();
    Bridge {
        _sockets: sockets,
        bridge,
        rx,
    }
}

/// A rule an earlier run installed for a profile that no longer exists.
fn stray() -> Rule {
    Rule {
        name: "850-profile:old:0000-x".into(),
        enabled: true,
        action: "allow".into(),
        duration: "always".into(),
        description: r#"{"snitchwatch":{"source":"profile","profile_id":"old","rule_id":"x"}}"#
            .into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "old.example".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn connect(bridge: &RunningBridge, snapshot: Vec<Rule>) -> mpsc::Receiver<Notification> {
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules: snapshot,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_responder(ListsPolicy::Accept, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    // Keep the mock's connection open for the test.
    std::mem::forget(daemon);
    seen
}

async fn next_command(seen: &mut mpsc::Receiver<Notification>) -> Notification {
    tokio::time::timeout(WAIT, seen.recv())
        .await
        .expect("no command reached the daemon")
        .unwrap()
}

async fn nothing_sent(seen: &mut mpsc::Receiver<Notification>) {
    let sent = tokio::time::timeout(Duration::from_millis(500), seen.recv()).await;
    assert!(sent.is_err(), "a command reached the daemon: {sent:?}");
}

async fn send(bridge: &RunningBridge, message: ClientMessage) {
    bridge.inbound_tx.send(message).await.unwrap();
}

/// Create profile "home" with one deny rule, and activate it.
async fn activate_home_with_a_rule(b: &mut Bridge) {
    send(
        &b.bridge,
        ClientMessage::CreateProfile {
            id: "home".into(),
            name: "Home".into(),
            network_matchers: vec![],
        },
    )
    .await;
    send(&b.bridge, add_rule("p1")).await;
    assert_eq!(result(&mut b.rx).await, RuleCommandOutcome::Ok);
    send(
        &b.bridge,
        ClientMessage::ActivateProfile { id: "home".into() },
    )
    .await;
}

fn add_rule(request_id: &str) -> ClientMessage {
    ClientMessage::AddProfileRule {
        profile_id: "home".into(),
        rule: ProfileRuleWire {
            id: "r1".into(),
            action: "deny".into(),
            operator: Some(
                serde_json::json!({ "type": "list", "operand": "list", "operands": [
                { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl",
                  "sensitive": true },
                { "type": "simple", "operand": "dest.host", "data": "ads.example" },
            ] }),
            ),
            ..Default::default()
        },
        request_id: Some(request_id.into()),
        reply: None,
    }
}

async fn result(rx: &mut broadcast::Receiver<ServerMessage>) -> RuleCommandOutcome {
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::RuleCommandResult { outcome, .. } = rx.recv().await.unwrap() {
                return outcome;
            }
        }
    })
    .await
    .expect("no result")
}

async fn profiles_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&[ProfileSummary], bool) -> bool,
) -> (Vec<ProfileSummary>, Option<String>) {
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetProfiles {
                profiles,
                applies_rules,
                not_applied_reason,
                ..
            } = rx.recv().await.unwrap()
            {
                if done(&profiles, applies_rules) {
                    return (profiles, not_applied_reason);
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"))
}

async fn installed(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ProfileSummary> {
    let (profiles, _) = profiles_until(rx, "rule installed", |profiles, applies| {
        applies
            && profiles.iter().any(|p| {
                p.rules
                    .iter()
                    .any(|r| r.enforcement == ENFORCEMENT_RULE_INSTALLED)
            })
    })
    .await;
    profiles
}

/// The Rules page's row for `name`, once a snapshot lists it.
async fn listed_row(b: &mut Bridge, name: &str) -> serde_json::Value {
    send(&b.bridge, ClientMessage::RequestSnapshot).await;
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetRules { rules } = b.rx.recv().await.unwrap() {
                if let Some(row) = rules.into_iter().find(|r| r["name"] == name) {
                    return row;
                }
            }
        }
    })
    .await
    .expect("no SetRules with the profile rule")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_active_profiles_rules_are_installed_and_strays_deleted() {
    let state = tempfile::tempdir().unwrap();
    let mut b = start_on(state.path(), BridgeMode::System).await;
    let mut seen = connect(&b.bridge, vec![stray()]).await;
    let purge = next_command(&mut seen).await;
    assert_eq!(purge.r#type, Action::DeleteRule as i32);
    assert_eq!(purge.rules[0].name, stray().name);

    activate_home_with_a_rule(&mut b).await;
    let install = next_command(&mut seen).await;
    assert_eq!(install.r#type, Action::ChangeRule as i32);
    let rule = &install.rules[0];
    assert_eq!(rule.name, RULE_NAME);
    assert!(!rule.precedence && rule.duration == "always");
    mock_opensnitchd::validate_rule_shape(rule).expect("a rule the daemon accepts");
    let profiles = installed(&mut b.rx).await;
    assert_eq!(profiles[0].rules[0].enforcement_reason, None);

    let row = listed_row(&mut b, RULE_NAME).await;
    assert_eq!(row["readOnlyReason"], PROFILE_MANAGED_REASON);

    send(&b.bridge, ClientMessage::DeactivateProfile).await;
    let removed = next_command(&mut seen).await;
    assert_eq!(removed.r#type, Action::DeleteRule as i32);
    assert_eq!(removed.rules[0].name, RULE_NAME);
    b.bridge.shutdown();
}

/// After a restart the active profile's rules, echoed in the daemon's
/// snapshot, are reported installed and not sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_finds_the_rules_in_place_and_resends_nothing() {
    let state = tempfile::tempdir().unwrap();
    let mut first = start_on(state.path(), BridgeMode::System).await;
    let mut seen = connect(&first.bridge, vec![]).await;
    activate_home_with_a_rule(&mut first).await;
    let mut echoed = next_command(&mut seen).await.rules[0].clone();
    installed(&mut first.rx).await;
    first.bridge.shutdown();

    echoed.created = 1_700_000_000;
    echoed.operator.as_mut().unwrap().operand = "list".into();
    let mut second = start_on(state.path(), BridgeMode::System).await;
    let mut seen = connect(&second.bridge, vec![echoed]).await;
    send(&second.bridge, ClientMessage::RequestSnapshot).await;
    installed(&mut second.rx).await;
    nothing_sent(&mut seen).await;
    second.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_per_user_bridge_installs_no_profile_rule_and_says_why() {
    let state = tempfile::tempdir().unwrap();
    let mut b = start_on(state.path(), BridgeMode::User).await;
    let mut seen = connect(&b.bridge, vec![]).await;
    activate_home_with_a_rule(&mut b).await;
    let (profiles, reason) = profiles_until(&mut b.rx, "not applied", |profiles, applies| {
        !applies
            && profiles
                .iter()
                .any(|p| p.rules.iter().any(|r| r.enforcement == "not_enforced"))
    })
    .await;
    assert!(reason.is_some_and(|r| r.contains("per-user")));
    assert!(profiles[0].rules[0].enforcement_reason.is_some());
    nothing_sent(&mut seen).await;
    b.bridge.shutdown();
}
