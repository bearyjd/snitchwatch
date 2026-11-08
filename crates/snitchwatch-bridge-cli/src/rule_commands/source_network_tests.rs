//! Adding or editing a `network` + `source.network` rule against a daemon
//! that reports 1.9 (`rule_policy::PolicyContext`): refused in the
//! editor's words, nothing sent; a 1.8 or unknown daemon still gets it.

use super::tests::{add, commands, refused, result, update};
use crate::test_daemon::*;
use serde_json::{json, Value};
use snitchwatch_bridge::daemon_config::DaemonVersion;
use snitchwatch_bridge::rule_policy::SOURCE_NETWORK_REFUSED;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use std::time::Duration;

fn network_rule(name: &str, operand: &str) -> Value {
    json!({
        "name": name, "enabled": true, "action": "allow", "duration": "always",
        "operator": { "type": "network", "operand": operand, "data": "192.168.0.0/16" },
    })
}

#[tokio::test]
async fn a_1_9_daemon_is_not_sent_a_new_source_network_rule() {
    let mut daemon = daemon(Vec::new());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    daemon
        .commands
        .daemon_version()
        .set(DaemonVersion::parse("mock-1.9.0"));
    assert!(commands
        .try_route(add(network_rule("100-lan", "source.network"), Some("a1")))
        .is_none());
    assert_eq!(
        refused(&result(&mut rx).await),
        vec![SOURCE_NETWORK_REFUSED.to_string()]
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        daemon.rx.try_recv().is_err(),
        "a command reached the daemon"
    );

    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));
    commands.try_route(add(network_rule("100-lan", "dest.network"), Some("a2")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(names(&seen), vec!["100-lan".to_string()]);
}

#[tokio::test]
async fn an_edit_to_source_network_is_refused_for_1_9_and_sent_for_1_8() {
    let existing = host_rule("100-x", "allow");
    let mut daemon = daemon(vec![existing]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let version = daemon.commands.daemon_version();
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));

    version.set(DaemonVersion::parse("1.9.0"));
    commands.try_route(update(
        "100-x",
        network_rule("100-x", "source.network"),
        Some("e1"),
    ));
    assert_eq!(
        refused(&result(&mut rx).await),
        vec![SOURCE_NETWORK_REFUSED.to_string()]
    );
    assert!(names(&seen).is_empty());

    version.set(DaemonVersion::parse("1.8.0"));
    commands.try_route(update(
        "100-x",
        network_rule("100-x", "source.network"),
        Some("e2"),
    ));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(names(&seen), vec!["100-x".to_string()]);
}

#[tokio::test]
async fn a_disabled_source_network_rule_is_accepted_by_1_9_and_enabling_it_is_refused() {
    let mut daemon = daemon(vec![host_rule("100-x", "allow")]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    daemon
        .commands
        .daemon_version()
        .set(DaemonVersion::parse("1.9.0"));
    let (seen, _) = respond(&mut daemon, |_| Some((true, String::new())));

    // The daemon does not compile a disabled rule, so adding or editing one
    // is as safe on 1.9 as on 1.8.
    let mut off = network_rule("100-lan", "source.network");
    off["enabled"] = json!(false);
    commands.try_route(add(off.clone(), Some("d1")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    off["name"] = json!("100-x");
    commands.try_route(update("100-x", off, Some("d2")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(names(&seen).len(), 2);

    // Turning it on is the step the daemon would refuse.
    commands.try_route(update(
        "100-x",
        network_rule("100-x", "source.network"),
        Some("d3"),
    ));
    assert_eq!(
        refused(&result(&mut rx).await),
        vec![SOURCE_NETWORK_REFUSED.to_string()]
    );
    assert_eq!(names(&seen).len(), 2);
}
