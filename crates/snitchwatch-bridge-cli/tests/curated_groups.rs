//! Enabling a curated group end to end: a GUI turns on the ids the bridge
//! listed under one group, and the mock daemon receives exactly that group's
//! rules, nothing from another group.

use std::collections::BTreeSet;

use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::curated::CuratedGroup;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::BridgeMode;
use snitchwatch_proto::protocol::{Action, Notification};
use tokio::sync::mpsc;

mod curated_support;
use curated_support::{connect_daemon, next_command, nothing_sent, send, start, Setup, WAIT};

/// The groups and entries of the bridge's next `SetCuratedDefaults`.
async fn listing(s: &mut Setup) -> (Vec<CuratedGroup>, Vec<CuratedDefaultSummary>) {
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::SetCuratedDefaults {
                entries, groups, ..
            }) = s.rx.recv().await
            {
                return (groups, entries);
            }
        }
    })
    .await
    .expect("no SetCuratedDefaults")
}

fn turn_on(ids: Vec<String>) -> ClientMessage {
    ClientMessage::SetCuratedDefaults {
        request_id: None,
        reply: None,
        ids,
        on: true,
    }
}

async fn installed_names(
    seen: &mut mpsc::Receiver<Notification>,
    count: usize,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for _ in 0..count {
        let command = next_command(seen).await;
        assert_eq!(command.r#type, Action::ChangeRule as i32);
        names.insert(command.rules[0].name.clone());
    }
    names
}

async fn enable_group_installs_exactly_its_rules(group: &str) {
    let mut s = start(BridgeMode::System).await;
    let (_daemon, mut seen): (MockOpensnitchd, _) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    nothing_sent(&mut seen).await;

    let (groups, entries) = listing(&mut s).await;
    assert!(groups.iter().any(|g| g.id == group), "{groups:?}");
    let members: Vec<&CuratedDefaultSummary> =
        entries.iter().filter(|e| e.group == group).collect();
    assert!(!members.is_empty());
    assert!(members.iter().all(|e| !e.broad && !e.on), "{members:?}");
    let others = entries.len() - members.len();
    assert!(others > 0, "the test needs entries in other groups");

    send(
        &s.bridge,
        turn_on(members.iter().map(|e| e.id.clone()).collect()),
    )
    .await;
    let expected: BTreeSet<String> = members
        .iter()
        .map(|e| format!("snitchwatch-default-{}", e.id))
        .collect();
    assert_eq!(installed_names(&mut seen, expected.len()).await, expected);
    // Nothing from another group follows.
    nothing_sent(&mut seen).await;
}

#[tokio::test]
async fn enabling_the_system_services_group_installs_exactly_its_rules() {
    enable_group_installs_exactly_its_rules("system-services").await;
}

#[tokio::test]
async fn enabling_the_tailscale_group_installs_exactly_its_rules() {
    enable_group_installs_exactly_its_rules("tailscale").await;
}

/// Every id the bridge lists under a group is a real entry of that group.
#[tokio::test]
async fn the_listing_names_a_group_for_every_entry() {
    let mut s = start(BridgeMode::System).await;
    let (_daemon, _seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    let (groups, entries) = listing(&mut s).await;
    let known: BTreeSet<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    assert!(entries.iter().all(|e| known.contains(e.group.as_str())));
}
