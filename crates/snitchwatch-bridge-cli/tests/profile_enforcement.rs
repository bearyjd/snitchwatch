//! Profile enforcement end to end (issue #46 Part 2): a bridge with a saved
//! state directory and the mock daemon. Activating a profile installs its
//! rules, a stray rule the bridge made earlier is deleted after the
//! daemon's snapshot, and a per-user bridge installs nothing and says why.

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

struct Setup {
    _sockets: tempfile::TempDir,
    _state: tempfile::TempDir,
    bridge: RunningBridge,
    rx: broadcast::Receiver<ServerMessage>,
}

async fn start(mode: BridgeMode) -> Setup {
    let sockets = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.path().canonicalize().unwrap()),
            blocklist_fetcher: None,
            mode,
        },
    )
    .await
    .unwrap();
    let rx = bridge.broadcast_tx.subscribe();
    Setup {
        _sockets: sockets,
        _state: state,
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

async fn send(bridge: &RunningBridge, message: ClientMessage) {
    bridge.inbound_tx.send(message).await.unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_active_profiles_rules_are_installed_and_strays_deleted() {
    let mut setup = start(BridgeMode::System).await;
    let (bridge, rx) = (&setup.bridge, &mut setup.rx);
    let mut seen = connect(bridge, vec![stray()]).await;

    let purge = next_command(&mut seen).await;
    assert_eq!(purge.r#type, Action::DeleteRule as i32);
    assert_eq!(purge.rules[0].name, "850-profile:old:0000-x");

    send(
        bridge,
        ClientMessage::CreateProfile {
            id: "home".into(),
            name: "Home".into(),
            network_matchers: vec![],
        },
    )
    .await;
    send(bridge, add_rule("p1")).await;
    assert_eq!(result(rx).await, RuleCommandOutcome::Ok);
    send(bridge, ClientMessage::ActivateProfile { id: "home".into() }).await;

    let install = next_command(&mut seen).await;
    assert_eq!(install.r#type, Action::ChangeRule as i32);
    let rule = &install.rules[0];
    assert_eq!(rule.name, "850-profile:home:0000-r1");
    assert!(!rule.precedence && rule.duration == "always");
    mock_opensnitchd::validate_rule_shape(rule).expect("a rule the daemon accepts");

    let (profiles, _) = profiles_until(rx, "rule installed", |profiles, applies| {
        applies
            && profiles.iter().any(|p| {
                p.rules
                    .iter()
                    .any(|r| r.enforcement == ENFORCEMENT_RULE_INSTALLED)
            })
    })
    .await;
    assert_eq!(profiles[0].rules[0].enforcement_reason, None);

    // The Rules page lists it read-only, managed on the Profiles page.
    send(bridge, ClientMessage::RequestSnapshot).await;
    let rules = tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetRules { rules } = rx.recv().await.unwrap() {
                if let Some(row) = rules
                    .into_iter()
                    .find(|r| r["name"] == "850-profile:home:0000-r1")
                {
                    return row;
                }
            }
        }
    })
    .await
    .expect("no SetRules with the profile rule");
    assert_eq!(rules["readOnlyReason"], PROFILE_MANAGED_REASON);

    send(bridge, ClientMessage::DeactivateProfile).await;
    let removed = next_command(&mut seen).await;
    assert_eq!(removed.r#type, Action::DeleteRule as i32);
    assert_eq!(removed.rules[0].name, "850-profile:home:0000-r1");
    setup.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_per_user_bridge_installs_no_profile_rule_and_says_why() {
    let mut setup = start(BridgeMode::User).await;
    let (bridge, rx) = (&setup.bridge, &mut setup.rx);
    let mut seen = connect(bridge, vec![]).await;
    send(
        bridge,
        ClientMessage::CreateProfile {
            id: "home".into(),
            name: "Home".into(),
            network_matchers: vec![],
        },
    )
    .await;
    send(bridge, add_rule("p1")).await;
    assert_eq!(result(rx).await, RuleCommandOutcome::Ok);
    send(bridge, ClientMessage::ActivateProfile { id: "home".into() }).await;
    let (profiles, reason) = profiles_until(rx, "not applied", |profiles, applies| {
        !applies
            && profiles
                .iter()
                .any(|p| p.rules.iter().any(|r| r.enforcement == "not_enforced"))
    })
    .await;
    assert!(reason.is_some_and(|r| r.contains("per-user")));
    assert!(profiles[0].rules[0].enforcement_reason.is_some());
    assert!(
        tokio::time::timeout(Duration::from_millis(500), seen.recv())
            .await
            .is_err(),
        "nothing reached the daemon"
    );
    setup.bridge.shutdown();
}
