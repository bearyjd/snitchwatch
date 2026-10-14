//! Tests for the cache's side of a refused rule command (tower r12): what
//! opensnitchd v1.8.0 holds after an `ERROR`, and the "file may remain"
//! marker. See `docs/superpowers/plans/2026-10-08-refused-delete-honesty.md`.

use super::*;
use snitchwatch_proto::protocol::Operator;
use tokio::sync::broadcast;

const T: i64 = 1_800_000_000;

fn rule(name: &str, enabled: bool, duration: &str) -> Rule {
    Rule {
        created: 1,
        name: name.to_string(),
        enabled,
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

fn listed(cache: &RulesCache) -> Vec<String> {
    cache.rules().expect("synced").keys().cloned().collect()
}

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn change(rule: Rule) -> Notification {
    Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule],
        ..Default::default()
    }
}

fn marked(cache: &RulesCache, name: &str) -> bool {
    cache.files_left().contains(name)
}

/// `Loader.Delete` drops the rule from memory before it removes the file,
/// and only removing the file can fail.
#[test]
fn a_refused_delete_unlists_the_rule_and_marks_its_file() {
    let mut cache = synced(vec![rule("a", true, "always"), rule("b", true, "always")]);
    let before = cache.revision();
    cache.apply_refused_at(&delete("a"), T);
    assert_eq!(listed(&cache), vec!["b"]);
    assert!(marked(&cache, "a"));
    assert!(!marked(&cache, "b"));
    assert!(cache.revision() > before);
}

#[test]
fn a_refused_delete_while_unknown_still_marks_the_file() {
    let mut cache = RulesCache::default();
    cache.apply_refused_at(&delete("a"), T);
    assert!(cache.is_unknown());
    assert!(marked(&cache, "a"));
}

#[test]
fn a_refused_delete_of_a_rule_left_out_for_size_unlists_it_too() {
    let mut cache = synced(Vec::new());
    cache.set_left_out(BTreeMap::from([("big".to_string(), 99_999)]));
    cache.apply_refused_at(&delete("big"), T);
    assert!(cache.left_out().is_empty());
    assert!(marked(&cache, "big"));
}

/// A snapshot listing the name means the daemon loaded its file again. One
/// that doesn't list it may be a redial without a restart: the file can
/// still be there.
#[test]
fn only_a_snapshot_listing_the_name_clears_the_marker() {
    let mut cache = synced(vec![rule("a", true, "always")]);
    cache.apply_refused_at(&delete("a"), T);
    cache.set_unknown();
    assert!(marked(&cache, "a"), "kept across a withdrawal");
    cache.replace_all(vec![rule("b", true, "always")]);
    assert!(marked(&cache, "a"), "kept by a snapshot without it");
    cache.replace_all(vec![rule("a", true, "always")]);
    assert!(!marked(&cache, "a"));

    cache.apply_refused_at(&delete("a"), T);
    cache.replace_all(Vec::new());
    cache.set_left_out(BTreeMap::from([("a".to_string(), 99_999)]));
    assert!(
        !marked(&cache, "a"),
        "a rule left out for size is listed too"
    );
}

/// Only an `always` change writes the file (`Replace` → `Save`). A delete of
/// a name not in memory is `OK` without touching the file, and a temporary
/// change writes none.
#[test]
fn only_a_confirmed_always_change_clears_the_marker() {
    let mut cache = synced(vec![rule("a", true, "always")]);
    cache.apply_refused_at(&delete("a"), T);

    cache.apply_confirmed_at(&delete("a"), T);
    assert!(marked(&cache, "a"), "a delete of a name not in memory");
    cache.apply_confirmed_at(&change(rule("a", true, "5m")), T);
    assert!(marked(&cache, "a"), "a temporary rule has no file");
    assert_eq!(listed(&cache), vec!["a"]);
    cache.apply_confirmed_at(&change(rule("a", true, "always")), T);
    assert!(!marked(&cache, "a"));
}

/// A refused disabled `always` change after the mark proves the daemon holds
/// the name as an `always` rule again (only `Save` failed). A delete `OK`
/// then went through `os.Remove` of its file: the file is gone (#120
/// item 4).
#[test]
fn a_delete_ok_after_the_daemon_held_the_name_again_clears_the_marker() {
    let mut cache = synced(vec![rule("a", true, "always")]);
    cache.apply_refused_at(&delete("a"), T);
    cache.apply_refused_at(&change(rule("a", false, "always")), T);
    assert!(marked(&cache, "a"), "its old file is still there");
    let before = cache.revision();
    cache.apply_confirmed_at(&delete("a"), T);
    assert!(!marked(&cache, "a"));
    assert!(cache.revision() > before, "the left-on-disk count changed");
}

/// The proof lasts only while the daemon still holds an `always` copy: a
/// temporary change (no file for `Delete` to remove) or a new refused
/// delete (out of memory again) ends it.
#[test]
fn the_daemon_holding_the_name_again_is_forgotten_when_it_stops() {
    for undo in [change(rule("a", true, "5m")), delete("a")] {
        let mut cache = synced(vec![rule("a", true, "always")]);
        cache.apply_refused_at(&delete("a"), T);
        cache.apply_refused_at(&change(rule("a", false, "always")), T);
        if undo.r#type == Action::DeleteRule as i32 {
            cache.apply_refused_at(&undo, T);
        } else {
            cache.apply_confirmed_at(&undo, T);
        }
        cache.apply_confirmed_at(&delete("a"), T);
        assert!(marked(&cache, "a"), "after {:?}", undo.r#type);
    }
}

/// A disabled rule isn't compiled, so for an `always` one only `Save` can
/// fail: the daemon holds the rule as sent, and its file is the old one.
#[test]
fn a_refused_disabled_always_change_is_what_the_daemon_holds() {
    let mut cache = synced(vec![rule("a", true, "always")]);
    let before = cache.revision();
    cache.apply_refused_at(&change(rule("a", false, "always")), T);
    let held = &cache.rules().unwrap()["a"];
    assert!(!held.enabled);
    assert_eq!(held.created, T, "restamped as the daemon stamps a change");
    assert!(cache.revision() > before);
    assert!(!marked(&cache, "a"));

    // Its old file stays: a marker already there is kept.
    cache.apply_refused_at(&delete("a"), T);
    cache.apply_refused_at(&change(rule("a", false, "always")), T);
    assert!(marked(&cache, "a"));
    assert_eq!(listed(&cache), vec!["a"]);
}

/// A compile error leaves the old rule; a `Save` failure the new one. The
/// bridge can't tell which from the command, so the cache stays as it was.
#[test]
fn a_refused_change_the_bridge_cannot_place_leaves_the_cache_as_it_was() {
    for sent in [
        rule("a", true, "always"),
        rule("a", true, "5m"),
        rule("a", false, "5m"),
        rule("new", true, "always"),
        // `Deserialize` refuses a rule without an operator before anything
        // changes, disabled and `always` or not.
        Rule {
            operator: None,
            ..rule("a", false, "always")
        },
    ] {
        let mut cache = synced(vec![rule("a", false, "always")]);
        let before = cache.revision();
        cache.apply_refused_at(&change(sent.clone()), T);
        assert_eq!(listed(&cache), vec!["a"], "{sent:?}");
        assert!(!cache.rules().unwrap()["a"].enabled, "{sent:?}");
        assert!(cache.rules().unwrap()["a"].operator.is_some(), "{sent:?}");
        assert_eq!(cache.revision(), before, "{sent:?}");
        assert!(cache.files_left().is_empty());
    }
}

#[test]
fn the_marker_is_bounded() {
    let mut cache = synced(Vec::new());
    for i in 0..MAX_FILES_LEFT + 10 {
        cache.apply_refused_at(&delete(&format!("r{i}")), T);
    }
    assert_eq!(cache.files_left().len(), MAX_FILES_LEFT);
}

#[test]
fn rules_not_shown_counts_the_files_left() {
    let mut cache = synced(vec![rule("a", true, "always")]);
    let ServerMessage::RulesNotShown { left_on_disk, .. } = cache.not_shown() else {
        panic!("not a RulesNotShown");
    };
    assert_eq!(left_on_disk, 0);
    cache.apply_refused_at(&delete("a"), T);
    let ServerMessage::RulesNotShown { left_on_disk, .. } = cache.not_shown() else {
        panic!("not a RulesNotShown");
    };
    assert_eq!(left_on_disk, 1);
}

#[test]
fn the_count_is_omitted_from_the_wire_when_zero() {
    let json = |left_on_disk| {
        serde_json::to_value(ServerMessage::RulesNotShown {
            too_large: 0,
            over_limit_total: None,
            listed: true,
            left_on_disk,
            count_mismatch: false,
        })
        .unwrap()
    };
    assert!(json(0).get("leftOnDisk").is_none());
    assert_eq!(json(2)["leftOnDisk"], 2);
}

fn set_rules(rx: &mut broadcast::Receiver<ServerMessage>) -> Option<Vec<String>> {
    loop {
        match rx.try_recv() {
            Ok(ServerMessage::SetRules { rules }) => {
                return Some(
                    rules
                        .iter()
                        .map(|r| r["name"].as_str().unwrap().to_string())
                        .collect(),
                )
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

/// `RulesSync::apply_refused` publishes like `apply_confirmed`: at once, or
/// once the last hold drops (the curated pass holds publishes).
#[tokio::test]
async fn a_refused_delete_is_published_after_the_hold_like_a_confirmed_one() {
    let (tx, mut rx) = broadcast::channel(64);
    let sync = RulesSync::new(tx);
    sync.cache()
        .lock()
        .unwrap()
        .replace_all(vec![rule("a", true, "always"), rule("b", true, "always")]);

    sync.apply_refused(&delete("a"));
    assert_eq!(set_rules(&mut rx), Some(vec!["b".to_string()]));

    let hold = sync.hold_publishes();
    sync.apply_refused(&delete("b"));
    assert_eq!(set_rules(&mut rx), None, "held");
    drop(hold);
    assert_eq!(set_rules(&mut rx), Some(Vec::new()));

    // An ERROR that changes nothing publishes nothing under a hold.
    let hold = sync.hold_publishes();
    sync.apply_refused(&change(rule("c", true, "always")));
    drop(hold);
    assert_eq!(set_rules(&mut rx), None);
}

fn staged_unix(
    rules: Vec<Rule>,
) -> (
    RulesSync,
    crate::daemon_commands::DaemonCommands,
    snitchwatch_proto::protocol::NotificationReply,
) {
    use crate::daemon_commands::{DaemonCommands, DaemonTransport};
    use snitchwatch_proto::protocol::{NotificationReply, NotificationReplyCode};

    let (tx, _) = broadcast::channel(64);
    let sync = RulesSync::new(tx);
    let commands = DaemonCommands::new(DaemonTransport::Unix, sync.clone());
    sync.stage(None, rules);
    let hello = NotificationReply {
        code: NotificationReplyCode::Ok as i32,
        ..Default::default()
    };
    (sync, commands, hello)
}

/// A Unix fallback reuses the retained Subscribe, not a new daemon list.
#[test]
fn a_retained_snapshot_keeps_a_later_refused_delete_marker() {
    // The name is evidence of a loaded file even when its rule was too large
    // to show, but only if the snapshot was observed after the refusal.
    for left_out in [false, true] {
        let mut a = rule("a", true, "always");
        if left_out {
            a.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
        }
        let (sync, commands, hello) = staged_unix(vec![a]);
        let (older, _older_rx) = commands.open_stream(None);
        commands.on_reply(older.id(), &hello);
        let (newer, _newer_rx) = commands.open_stream(None);
        commands.on_reply(newer.id(), &hello);
        sync.apply_refused(&delete("a"));
        assert!(marked(&lock(&sync.cache), "a"));

        drop(newer); // readopt the same snapshot for the fallback stream.
        let cache = lock(&sync.cache);
        assert!(!cache.is_unknown(), "the fallback adopted its list");
        assert!(
            marked(&cache, "a"),
            "a retained snapshot predating the refusal cannot prove the file reloaded (left_out={left_out})"
        );
        assert!(matches!(
            cache.not_shown(),
            ServerMessage::RulesNotShown {
                left_on_disk: 1,
                ..
            }
        ));
        drop(cache);
        drop(older);
    }
}

#[test]
fn a_delayed_first_adoption_keeps_a_later_refusal_marker() {
    let (sync, commands, hello) = staged_unix(vec![rule("a", true, "always")]);
    sync.apply_refused(&delete("a"));
    let (stream, _rx) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello);
    assert!(marked(&lock(&sync.cache), "a"));
}

#[test]
fn a_new_snapshot_only_clears_markers_for_names_it_lists() {
    for left_out in [false, true] {
        let (sync, commands, hello) = staged_unix(Vec::new());
        sync.apply_refused(&delete("a"));
        sync.apply_refused(&delete("b"));
        let mut a = rule("a", true, "always");
        if left_out {
            a.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
        }
        sync.stage(None, vec![a]);
        let (stream, _rx) = commands.open_stream(None);
        commands.on_reply(stream.id(), &hello);
        let cache = lock(&sync.cache);
        assert!(
            !marked(&cache, "a"),
            "a new list proves a loaded (left_out={left_out})"
        );
        assert!(
            marked(&cache, "b"),
            "an absent name proves nothing about its file"
        );
        drop(cache);
        drop(stream);
    }
}

#[test]
fn a_repeated_refusal_is_newer_than_the_snapshot_between_refusals() {
    let (sync, commands, hello) = staged_unix(Vec::new());
    sync.apply_refused(&delete("a"));
    sync.stage(None, vec![rule("a", true, "always")]);
    sync.apply_refused(&delete("a"));
    let (stream, _rx) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello);
    assert!(marked(&lock(&sync.cache), "a"));
}

#[test]
fn confirmed_file_operations_after_readoption_keep_their_marker_semantics() {
    let (sync, commands, hello) = staged_unix(vec![rule("a", true, "always")]);
    let (older, _older_rx) = commands.open_stream(None);
    commands.on_reply(older.id(), &hello);
    let (newer, _newer_rx) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello);
    sync.apply_refused(&delete("a"));
    drop(newer);
    assert!(marked(&lock(&sync.cache), "a"));

    // The retained list may show a again, but stock Delete's OK for a name
    // already gone from daemon memory still cannot prove its file was removed.
    sync.apply_confirmed(&delete("a"));
    assert!(marked(&lock(&sync.cache), "a"));
    sync.apply_confirmed(&change(rule("a", true, "5m")));
    assert!(
        marked(&lock(&sync.cache), "a"),
        "a temporary change writes no file"
    );
    sync.apply_confirmed(&change(rule("a", true, "always")));
    assert!(
        !marked(&lock(&sync.cache), "a"),
        "a confirmed always change rewrites the file"
    );
}
