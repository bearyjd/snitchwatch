use super::*;
use snitchwatch_proto::protocol::{Event, Rule};
use std::sync::Mutex as StdMutex;

fn rule(name: &str) -> Rule {
    Rule {
        name: name.to_string(),
        ..Default::default()
    }
}

fn ev(name: &str) -> Event {
    Event {
        rule: Some(rule(name)),
        unixnano: 1_700_000_000_000_000_000,
        ..Default::default()
    }
}

fn synced(names: &[&str]) -> SharedRulesCache {
    let mut cache = RulesCache::default();
    cache.replace_all(names.iter().map(|n| rule(n)).collect());
    Arc::new(StdMutex::new(cache))
}

fn handle() -> (RuleHitsHandle, broadcast::Receiver<ServerMessage>) {
    let (tx, rx) = broadcast::channel(64);
    (RuleHitsHandle::new(tx), rx)
}

struct View {
    since: Option<i64>,
    lossy: bool,
    storage: StorageStatus,
    hits: Vec<(String, u64)>,
}

fn view(message: ServerMessage) -> View {
    match message {
        ServerMessage::RuleHits {
            since_unix_ms,
            lossy,
            storage,
            hits,
            ..
        } => View {
            since: since_unix_ms,
            lossy,
            storage,
            hits: hits.into_iter().map(|h| (h.name, h.count)).collect(),
        },
        other => panic!("expected RuleHits, got {other:?}"),
    }
}

fn drain(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn counts(handle: &RuleHitsHandle) -> Vec<(String, u64)> {
    view(handle.message()).hits
}

/// Lets a spawned task run to its next await.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn fifty_pings_in_one_period_make_one_broadcast_with_all_of_them() {
    let (hits, mut rx) = handle();
    let rules = synced(&["a"]);
    let _ticker = hits.spawn_ticker();
    settle().await;
    assert!(drain(&mut rx).is_empty(), "nothing has changed yet");

    for _ in 0..50 {
        hits.record(&[ev("a")], 10, &rules);
        tokio::time::advance(Duration::from_millis(90)).await;
        settle().await;
    }
    assert!(drain(&mut rx).is_empty(), "4.5 s in: the period isn't over");

    tokio::time::advance(Duration::from_millis(600)).await;
    settle().await;
    let sent = drain(&mut rx);
    assert_eq!(sent.len(), 1, "at most one per period");
    assert_eq!(
        view(sent.into_iter().next().unwrap()).hits,
        vec![("a".to_string(), 50)]
    );

    tokio::time::advance(BROADCAST_PERIOD * 3).await;
    settle().await;
    assert!(drain(&mut rx).is_empty(), "an idle tick sends nothing");

    hits.record(&[ev("a")], 11, &rules);
    tokio::time::advance(BROADCAST_PERIOD).await;
    settle().await;
    let sent = drain(&mut rx);
    assert_eq!(sent.len(), 1);
    assert_eq!(view(sent.into_iter().next().unwrap()).hits[0].1, 51);
}

#[test]
fn the_snapshot_answer_is_sent_even_before_the_first_ping() {
    let (hits, _rx) = handle();
    let (tx, mut out) = broadcast::channel(4);
    hits.announce(&tx);
    let v = view(out.try_recv().expect("an answer"));
    assert_eq!(v.since, None);
    assert!(!v.lossy);
    assert!(v.hits.is_empty());
    assert!(!v.storage.persistent);
}

#[test]
fn counts_use_the_rule_cache_and_adopt_at_the_commit() {
    let (hits, _rx) = handle();
    let unknown: SharedRulesCache = Arc::default();
    hits.record(&[ev("a"), ev("a")], 10, &unknown);
    assert!(counts(&hits).is_empty(), "no snapshot yet");

    let mut cache = RulesCache::default();
    cache.replace_all(vec![rule("a")]);
    hits.adopt_snapshot(&cache);
    assert_eq!(counts(&hits), vec![("a".to_string(), 2)]);
}

#[test]
fn the_daemons_max_events_sets_the_threshold_for_an_incomplete_batch() {
    let (hits, _rx) = handle();
    let rules = synced(&["a"]);
    let batch: Vec<Event> = (0..9).map(|_| ev("a")).collect();
    hits.set_max_events(20);
    hits.record(&batch, 10, &rules);
    assert!(!view(hits.message()).lossy);
    hits.set_max_events(10);
    hits.record(&batch, 11, &rules);
    assert!(view(hits.message()).lossy);
}

fn state_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn counts_survive_a_restart_through_the_file() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let rules = synced(&["a", "b"]);

    let (first, _rx) = handle();
    first.attach_file(path.clone());
    assert!(view(first.message()).storage.persistent);
    first.record(&[ev("a"), ev("a"), ev("b")], 10, &rules);
    let started = view(first.message()).since;
    assert!(started.is_some());
    first.save_now();

    let (second, _rx) = handle();
    second.attach_file(path);
    let before_commit = view(second.message());
    assert_eq!(before_commit.since, started, "the persisted start time");
    assert!(before_commit.lossy, "the bridge was down in between");
    assert!(
        before_commit.hits.is_empty(),
        "nothing is shown before a snapshot"
    );
    second.adopt_snapshot(&rules.lock().unwrap());
    assert_eq!(
        counts(&second),
        vec![("a".to_string(), 2), ("b".to_string(), 1)]
    );
}

#[test]
fn only_rules_in_the_first_snapshot_come_back_and_the_rest_are_pruned_for_good() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (first, _rx) = handle();
    first.attach_file(path.clone());
    first.record(&[ev("a"), ev("gone")], 10, &synced(&["a", "gone"]));
    first.save_now();

    let (second, _rx) = handle();
    second.attach_file(path.clone());
    second.adopt_snapshot(&synced(&["a"]).lock().unwrap());
    assert_eq!(counts(&second), vec![("a".to_string(), 1)]);
    second.record(&[ev("a")], 20, &synced(&["a"]));
    second.save_now();

    let (third, _rx) = handle();
    third.attach_file(path);
    third.adopt_snapshot(&synced(&["a", "gone"]).lock().unwrap());
    assert_eq!(
        counts(&third),
        vec![("a".to_string(), 2)],
        "gone stayed gone"
    );
}

#[test]
fn counts_not_yet_checked_survive_a_save_before_the_daemon_connects() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (first, _rx) = handle();
    first.attach_file(path.clone());
    first.record(&[ev("a")], 10, &synced(&["a"]));
    first.save_now();

    // A bridge that runs a while with no daemon, saving as it goes.
    let (second, _rx) = handle();
    second.attach_file(path.clone());
    second.record(&[], 1, &synced(&[]));
    second.save_now();
    second.save_now();

    let (third, _rx) = handle();
    third.attach_file(path);
    third.adopt_snapshot(&synced(&["a"]).lock().unwrap());
    assert_eq!(counts(&third), vec![("a".to_string(), 1)]);
}

#[test]
fn only_changed_counts_are_saved() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, _rx) = handle();
    hits.attach_file(path.clone());
    let rules = synced(&["a"]);
    hits.save_now();
    assert!(!path.exists(), "counting hasn't started");
    hits.record(&[ev("a")], 10, &rules);
    hits.save_now();
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
    hits.save_now();
    assert!(!path.exists(), "nothing changed, so nothing is written");
    hits.record(&[ev("a")], 11, &rules);
    hits.save_now();
    assert!(path.exists());
}

#[test]
fn without_a_file_the_counts_stay_in_memory_and_clients_are_told() {
    let (hits, _rx) = handle();
    hits.set_storage(StorageStatus {
        persistent: false,
        reason: Some("state directory /x is not a directory".into()),
        unreadable: false,
    });
    hits.record(&[ev("a")], 10, &synced(&["a"]));
    hits.save_now();
    let v = view(hits.message());
    assert_eq!(v.hits, vec![("a".to_string(), 1)]);
    assert!(!v.storage.persistent);
    assert_eq!(
        v.storage.reason.as_deref(),
        Some("state directory /x is not a directory")
    );
}

#[test]
fn a_file_that_cannot_be_read_is_left_alone_and_the_counts_stay_in_memory() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    std::fs::write(&path, b"{ not json").unwrap();
    let (hits, _rx) = handle();
    hits.attach_file(path.clone());
    let v = view(hits.message());
    assert!(!v.storage.persistent);
    assert!(v.storage.reason.unwrap().contains("rule hit counts file"));

    hits.record(&[ev("a")], 10, &synced(&["a"]));
    hits.save_now();
    assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
    assert_eq!(counts(&hits), vec![("a".to_string(), 1)]);
}

#[test]
fn a_failing_save_turns_persistence_off_with_the_reason_and_a_good_one_turns_it_back_on() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, mut rx) = handle();
    hits.attach_file(path.clone());
    let rules = synced(&["a"]);
    // A directory where the file belongs: the save is refused.
    std::fs::create_dir(&path).unwrap();
    hits.record(&[ev("a")], 10, &rules);
    hits.save_now();
    let v = view(hits.message());
    assert!(!v.storage.persistent);
    assert!(v.storage.reason.unwrap().contains("couldn't save"));
    drain(&mut rx);
    hits.flush();
    assert_eq!(drain(&mut rx).len(), 1, "clients hear about it");

    std::fs::remove_dir(&path).unwrap();
    hits.save_now();
    assert!(view(hits.message()).storage.persistent);
    assert!(path.is_file());
}

#[tokio::test(start_paused = true)]
async fn the_ticker_saves_changed_counts_every_five_minutes() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, _rx) = handle();
    hits.attach_file(path.clone());
    hits.record(&[ev("a")], 10, &synced(&["a"]));
    let _ticker = hits.spawn_ticker();
    settle().await;

    // One period at a time: a tick that comes late delays the next.
    let periods = SAVE_PERIOD.as_secs() / BROADCAST_PERIOD.as_secs();
    for _ in 0..periods - 2 {
        tokio::time::advance(BROADCAST_PERIOD).await;
        settle().await;
    }
    assert!(!path.exists(), "not yet");

    for _ in 0..2 {
        tokio::time::advance(BROADCAST_PERIOD).await;
        settle().await;
    }
    // The save runs on the blocking pool, off the paused clock.
    for _ in 0..200 {
        if path.exists() {
            break;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "saved at five minutes");
}
