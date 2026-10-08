//! The [`RulesCache`] revision (rule import's stale-preview check, roadmap
//! P2.7) and [`RulesSync::hold_publishes`].

use super::*;
use snitchwatch_proto::protocol::Operator;

fn rule(name: &str, duration: &str, created: i64) -> Rule {
    Rule {
        created,
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: duration.to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn synced(rules: Vec<Rule>) -> RulesCache {
    let mut cache = RulesCache::default();
    cache.replace_all(rules);
    cache
}

#[test]
fn every_mutation_bumps_the_revision() {
    let mut cache = RulesCache::default();
    assert!(cache.is_unknown());
    let mut last = cache.revision();
    let mut bumped = |cache: &RulesCache, what: &str| {
        assert!(cache.revision() > last, "{what} did not bump the revision");
        last = cache.revision();
    };

    cache.replace_all(vec![rule("a", "always", 1), rule("old", "5m", 1)]);
    bumped(&cache, "replace_all");
    cache.upsert(rule("b", "always", 1));
    bumped(&cache, "upsert");
    cache.remove("b");
    bumped(&cache, "remove");
    cache.apply_confirmed(&Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule("c", "always", 0)],
        ..Default::default()
    });
    bumped(&cache, "apply_confirmed CHANGE_RULE");
    cache.apply_confirmed(&Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![rule("c", "always", 0)],
        ..Default::default()
    });
    bumped(&cache, "apply_confirmed DELETE_RULE");
    assert!(!cache.prune_expired(1_000).is_empty());
    bumped(&cache, "prune_expired");
    cache.set_unknown();
    bumped(&cache, "set_unknown");
    assert!(cache.is_unknown());
    cache.replace_all(Vec::new());
    bumped(&cache, "replace_all after Unknown (never reset)");
}

#[test]
fn a_no_op_keeps_the_revision() {
    let mut cache = synced(vec![rule("a", "always", 1)]);
    let revision = cache.revision();
    assert!(cache.prune_expired(1_000).is_empty(), "nothing to prune");
    assert_eq!(cache.revision(), revision);

    let mut unknown = RulesCache::default();
    let revision = unknown.revision();
    unknown.upsert(rule("a", "always", 1));
    unknown.remove("a");
    unknown.set_unknown();
    assert_eq!(unknown.revision(), revision, "no-ops while Unknown");
}

#[tokio::test(start_paused = true)]
async fn the_expiry_tick_bumps_the_revision() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let cache: SharedRulesCache = Arc::new(StdMutex::new(synced(vec![rule(
        "expired",
        "5m",
        now - 301,
    )])));
    let before = lock(&cache).revision();
    let (tx, mut rx) = broadcast::channel(4);
    let tick = tokio::spawn(prune_expired_rules_every(
        Duration::from_secs(30),
        Arc::downgrade(&cache),
        tx.clone(),
        RuleHitsHandle::new(tx),
    ));
    // Bounded, so a mutant that prunes nothing fails instead of hanging.
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("the tick never published")
        .unwrap();
    assert!(lock(&cache).revision() > before);
    tick.abort();
}

#[test]
fn withdraw_bumps_the_revision_and_broadcasts_only_once() {
    let (tx, mut rx) = broadcast::channel(8);
    let sync = RulesSync::new(tx);
    lock(&sync.cache).replace_all(vec![rule("a", "always", 1)]);
    let before = lock(&sync.cache).revision();

    sync.withdraw();
    assert!(lock(&sync.cache).is_unknown());
    assert!(lock(&sync.cache).revision() > before);
    assert!(matches!(rx.try_recv(), Ok(ServerMessage::SetRules { rules }) if rules.is_empty()));
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::RulesNotShown { .. })
    ));

    sync.withdraw();
    assert!(rx.try_recv().is_err(), "a second withdraw is a no-op");
}

#[test]
fn held_publishes_coalesce_into_one_set_rules() {
    let (tx, mut rx) = broadcast::channel(8);
    let sync = RulesSync::new(tx);
    lock(&sync.cache).replace_all(Vec::new());
    let change = |name: &str| Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule(name, "always", 0)],
        ..Default::default()
    };

    let hold = sync.hold_publishes();
    let nested = sync.hold_publishes();
    sync.apply_confirmed(&change("a"));
    sync.apply_confirmed(&change("b"));
    drop(nested);
    assert!(rx.try_recv().is_err(), "nothing published while held");

    drop(hold);
    match rx.try_recv() {
        Ok(ServerMessage::SetRules { rules }) => assert_eq!(rules.len(), 2),
        other => panic!("expected one SetRules, got {other:?}"),
    }
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::RulesNotShown { .. })
    ));
    assert!(rx.try_recv().is_err(), "exactly one");

    sync.apply_confirmed(&change("c"));
    assert!(matches!(rx.try_recv(), Ok(ServerMessage::SetRules { .. })));
}

/// A daemon rule over the field limits is left out of the list, but its
/// name and size are kept: an import must not overwrite it unseen (P2.7
/// review M1), and the snapshot-size estimate must count it (M3).
#[test]
fn rules_left_out_of_a_snapshot_keep_their_name_and_size() {
    let mut long = rule("long", "always", 1);
    long.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
    let mut nameless = rule("", "always", 1);
    nameless.description = long.description.clone();
    let snapshot = bounded_snapshot(vec![rule("kept", "always", 1), long, nameless]);
    assert_eq!(snapshot.rules.len(), 1);
    assert_eq!(snapshot.left_out.len(), 2, "every left-out rule is counted");
    assert!(snapshot.left_out["long"] > MAX_RULE_FIELD_BYTES);

    let mut cache = RulesCache::default();
    cache.replace_all(snapshot.rules);
    cache.set_left_out(snapshot.left_out);
    assert!(cache.left_out().contains_key("long"));
    assert!(!cache.rules().unwrap().contains_key("long"));

    // A confirmed change or delete of that name means the daemon now holds
    // what the bridge sent, or nothing.
    cache.upsert(rule("long", "always", 0));
    assert!(!cache.left_out().contains_key("long"));
    let mut again = RulesCache::default();
    again.replace_all(Vec::new());
    again.set_left_out([("big".to_string(), 20_000)].into());
    let before = again.revision();
    again.remove("big");
    assert!(again.left_out().is_empty() && again.revision() > before);
    again.set_unknown();
    assert!(again.left_out().is_empty());
}
