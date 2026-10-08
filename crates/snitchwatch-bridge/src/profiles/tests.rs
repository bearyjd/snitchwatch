//! Tests for [`super::ProfilesManager`]: profile actions, enforcement
//! passes and statuses (issue #46 Part 2), and auto vs. manual activation
//! (issue #82).

use super::test_helpers::{CapturingRuleSink, FakeNetworkWatcher};
use super::*;
use std::time::Duration;

fn manager() -> ProfilesManager {
    ProfilesManager::new(Arc::new(ProfileStore::open_in_memory().unwrap()))
}

fn with_sink() -> (ProfilesManager, Arc<CapturingRuleSink>) {
    let sink = Arc::new(CapturingRuleSink::default());
    (manager().with_rule_sink(sink.clone()), sink)
}

fn host_rule(id: &str, host: &str) -> ProfileRule {
    ProfileRule {
        id: id.into(),
        action: "deny".into(),
        operand: "dest.host".into(),
        data: host.into(),
        operator: None,
    }
}

fn active(mgr: &ProfilesManager) -> Option<String> {
    mgr.store().get_active().unwrap().map(|p| p.id)
}

async fn two_profiles(mgr: &ProfilesManager) {
    mgr.create_profile("home", "Home", vec!["Home*".into()])
        .await
        .unwrap();
    mgr.create_profile("office", "Office", vec!["Office*".into()])
        .await
        .unwrap();
}

#[tokio::test]
async fn create_profile_emits_profiles_changed_event() {
    let mgr = manager();
    let mut rx = mgr.subscribe();
    mgr.create_profile("home", "At Home", vec!["Home*".into()])
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await.unwrap(),
        ProfileEvent::ProfilesChanged
    ));
    assert_eq!(mgr.store().list_profiles().unwrap().len(), 1);
}

/// Activating only changes the store and asks for a pass; the pass installs
/// the profile's rules and records each one's status.
#[tokio::test]
async fn a_pass_installs_the_active_profiles_rules_and_records_their_status() {
    let (mgr, sink) = with_sink();
    mgr.create_profile("home", "Home", vec![]).await.unwrap();
    mgr.add_rule("home", host_rule("r1", "nas.local"))
        .await
        .unwrap();
    let mut rx = mgr.subscribe();
    mgr.activate("home").await.unwrap();
    assert!(matches!(
        rx.recv().await.unwrap(),
        ProfileEvent::ActiveProfileChanged { profile_id: Some(ref id) } if id == "home"
    ));
    assert!(
        sink.calls.lock().unwrap().is_empty(),
        "activation sends nothing"
    );
    assert_eq!(mgr.rule_status("home", "r1"), Some(Enforcement::Pending));

    mgr.enforce().await;
    assert_eq!(sink.last().unwrap(), vec!["850-profile:home:0000-r1"]);
    assert!(matches!(
        mgr.rule_status("home", "r1"),
        Some(Enforcement::RuleInstalled { .. })
    ));
    assert_eq!(
        mgr.rule_status("office", "r1"),
        None,
        "only the active profile"
    );
}

/// A saved rule the policy refuses is never handed to the sink, and says
/// why; the profile's other rules still go in.
#[tokio::test]
async fn a_refused_saved_rule_is_not_installed_and_says_why() {
    let (mgr, sink) = with_sink();
    let store = mgr.store().clone();
    store
        .upsert_profile(&Profile {
            id: "home".into(),
            name: "Home".into(),
            network_matchers: vec![],
            rules: vec![host_rule("r1", "a.example"), host_rule("r2", "")],
            active: false,
        })
        .unwrap();
    mgr.activate("home").await.unwrap();
    mgr.enforce().await;
    assert_eq!(sink.last().unwrap(), vec!["850-profile:home:0000-r1"]);
    match mgr.rule_status("home", "r2") {
        Some(Enforcement::NotEnforced { reason }) => {
            assert!(reason.starts_with("Not installed: "), "{reason}");
            assert!(reason.contains("blank host name"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn switching_or_deactivating_changes_what_a_pass_wants() {
    let (mgr, sink) = with_sink();
    two_profiles(&mgr).await;
    mgr.add_rule("home", host_rule("r1", "a.example"))
        .await
        .unwrap();
    mgr.add_rule("office", host_rule("o1", "b.example"))
        .await
        .unwrap();
    mgr.activate("home").await.unwrap();
    mgr.enforce().await;
    mgr.activate("office").await.unwrap();
    assert_eq!(
        mgr.rule_status("office", "o1"),
        Some(Enforcement::Pending),
        "a new activation starts pending"
    );
    mgr.enforce().await;
    assert_eq!(sink.last().unwrap(), vec!["850-profile:office:0000-o1"]);
    mgr.deactivate().await.unwrap();
    mgr.enforce().await;
    assert_eq!(sink.last().unwrap(), Vec::<String>::new());
    assert_eq!(active(&mgr), None);
}

#[tokio::test]
async fn deleting_the_active_profile_deactivates_it() {
    let (mgr, sink) = with_sink();
    mgr.create_profile("home", "Home", vec![]).await.unwrap();
    mgr.add_rule("home", host_rule("r1", "a.example"))
        .await
        .unwrap();
    mgr.activate("home").await.unwrap();
    mgr.delete_profile("home").await.unwrap();
    mgr.enforce().await;
    assert_eq!(sink.last().unwrap(), Vec::<String>::new());
    assert_eq!(mgr.store().manual_choice().unwrap(), None);
}

/// `AddProfileRule` is a system boundary: a rule that isn't installable,
/// a bad id, or one rule too many is refused and nothing is stored.
#[tokio::test]
async fn add_rule_refuses_what_the_bridge_wouldnt_install() {
    let mgr = manager();
    mgr.create_profile("home", "Home", vec![]).await.unwrap();
    let relative = ProfileRule {
        operand: "process.path".into(),
        data: "curl".into(),
        ..host_rule("r1", "")
    };
    for rule in [relative, host_rule("a/b", "x.example"), host_rule("r2", "")] {
        assert!(matches!(
            mgr.add_rule("home", rule).await,
            Err(ProfilesError::Refused(_))
        ));
    }
    for i in 0..MAX_RULES_PER_PROFILE {
        mgr.add_rule("home", host_rule(&format!("r{i}"), "x.example"))
            .await
            .unwrap();
    }
    assert!(matches!(
        mgr.add_rule("home", host_rule("one-more", "x.example"))
            .await,
        Err(ProfilesError::Refused(_))
    ));
    let stored = mgr.store().get_profile("home").unwrap().unwrap().rules;
    assert_eq!(stored.len(), MAX_RULES_PER_PROFILE);
}

#[tokio::test]
async fn auto_switch_activates_the_matching_profile_on_a_network_change() {
    let mgr = manager();
    two_profiles(&mgr).await;
    mgr.on_network_observed(Some("Home-5G".into()))
        .await
        .unwrap();
    assert_eq!(active(&mgr).as_deref(), Some("home"));
    mgr.on_network_observed(Some("Office-Guest".into()))
        .await
        .unwrap();
    assert_eq!(active(&mgr).as_deref(), Some("office"));
    mgr.on_network_observed(Some("Coffee-Shop".into()))
        .await
        .unwrap();
    assert_eq!(
        active(&mgr).as_deref(),
        Some("office"),
        "no match: left as is"
    );
}

#[tokio::test]
async fn a_manual_choice_holds_on_its_network_until_the_network_changes() {
    let mgr = manager();
    two_profiles(&mgr).await;
    mgr.note_network(Some("Home-5G".into()));
    mgr.on_network_observed(Some("Home-5G".into()))
        .await
        .unwrap();
    mgr.activate("office").await.unwrap();
    mgr.on_network_observed(Some("Home-5G".into()))
        .await
        .unwrap();
    assert_eq!(active(&mgr).as_deref(), Some("office"));
    mgr.on_network_observed(Some("Home-Guest".into()))
        .await
        .unwrap();
    assert_eq!(
        active(&mgr).as_deref(),
        Some("home"),
        "a new network decides"
    );
}

/// Issue #82: the first reading after a restart, on the network the manual
/// choice was made on, keeps it; another network lets auto-switch decide.
#[tokio::test]
async fn a_manual_choice_survives_a_restart_on_the_same_network() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.sqlite3");
    let open = || ProfilesManager::new(Arc::new(ProfileStore::open(&path).unwrap()));
    let before = open();
    two_profiles(&before).await;
    before.note_network(Some("Home-5G".into()));
    before
        .on_network_observed(Some("Home-5G".into()))
        .await
        .unwrap();
    before.activate("office").await.unwrap();
    drop(before);

    let same = open();
    same.on_network_observed(Some("Home-5G".into()))
        .await
        .unwrap();
    assert_eq!(
        active(&same).as_deref(),
        Some("office"),
        "kept after a restart"
    );
    drop(same);

    let moved = open();
    moved
        .on_network_observed(Some("Home-Guest".into()))
        .await
        .unwrap();
    assert_eq!(
        active(&moved).as_deref(),
        Some("home"),
        "another network decides"
    );
    assert_eq!(moved.store().manual_choice().unwrap(), None);
}

/// A click while a new network is still settling is saved with that
/// network, so the settled reading doesn't override it.
#[tokio::test]
async fn a_click_during_the_settle_time_is_saved_with_the_newest_network() {
    let mgr = manager();
    two_profiles(&mgr).await;
    mgr.note_network(Some("Office-Guest".into()));
    mgr.activate("home").await.unwrap();
    mgr.on_network_observed(Some("Office-Guest".into()))
        .await
        .unwrap();
    assert_eq!(active(&mgr).as_deref(), Some("home"));
}

/// A flapping network is acted on once it settles, not on every change.
#[tokio::test(start_paused = true)]
async fn a_flapping_network_is_acted_on_once_it_settles() {
    let mgr = Arc::new(manager());
    two_profiles(&mgr).await;
    let mut events = mgr.subscribe();
    let (watcher, tx) = FakeNetworkWatcher::new(None);
    let handle = mgr.clone().spawn_auto_switch(watcher);
    tokio::time::sleep(Duration::from_millis(10)).await;
    for network in ["Home-5G", "Office-Guest", "Home-5G", "Office-Guest"] {
        tx.send(Some(network.into())).unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(active(&mgr), None, "nothing while it flaps");
    tokio::time::sleep(tasks::NETWORK_SETTLE).await;
    assert_eq!(active(&mgr).as_deref(), Some("office"));
    let mut switches = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, ProfileEvent::ActiveProfileChanged { .. }) {
            switches += 1;
        }
    }
    assert_eq!(switches, 1);
    handle.abort();
}

#[tokio::test]
async fn the_enforcer_runs_a_pass_on_requests_and_on_each_rules_snapshot() {
    let (mgr, sink) = with_sink();
    let mgr = Arc::new(mgr);
    let (synced_tx, synced_rx) = tokio::sync::watch::channel(0u64);
    let handles = mgr.clone().spawn_enforcer(Some(synced_rx));
    let passes = || sink.calls.lock().unwrap().len();
    let wait_for = |n: usize| async move {
        for _ in 0..200 {
            if passes() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("only {} passes, wanted {n}", passes());
    };
    wait_for(1).await;
    synced_tx.send(1).unwrap();
    wait_for(2).await;
    mgr.create_profile("home", "Home", vec![]).await.unwrap();
    mgr.activate("home").await.unwrap();
    wait_for(3).await;
    for handle in handles {
        handle.abort();
    }
}
