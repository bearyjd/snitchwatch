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

/// The state a bridge starts with (its storage status) is in every snapshot
/// answer; broadcasting it too would only land between other messages.
#[tokio::test(start_paused = true)]
async fn the_ticker_does_not_broadcast_the_state_it_starts_with() {
    let (hits, mut rx) = handle();
    hits.set_storage(StorageStatus {
        persistent: false,
        reason: Some("no state directory".into()),
        unreadable: false,
    });
    let _ticker = hits.spawn_ticker();
    settle().await;
    tokio::time::advance(BROADCAST_PERIOD * 2).await;
    settle().await;
    assert!(drain(&mut rx).is_empty(), "nothing changed since the start");

    hits.record(&[ev("a")], 10, 1, &synced(&["a"]));
    tokio::time::advance(BROADCAST_PERIOD).await;
    settle().await;
    assert_eq!(drain(&mut rx).len(), 1, "a change after the start is sent");
}

#[tokio::test(start_paused = true)]
async fn fifty_pings_in_one_period_make_one_broadcast_with_all_of_them() {
    let (hits, mut rx) = handle();
    let rules = synced(&["a"]);
    let _ticker = hits.spawn_ticker();
    settle().await;
    assert!(drain(&mut rx).is_empty(), "nothing has changed yet");

    for i in 1..=50 {
        hits.record(&[ev("a")], 10, i, &rules);
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

    hits.record(&[ev("a")], 11, 51, &rules);
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
    hits.record(&[ev("a"), ev("a")], 10, 2, &unknown);
    assert!(counts(&hits).is_empty(), "no snapshot yet");

    let mut cache = RulesCache::default();
    cache.replace_all(vec![rule("a")]);
    hits.adopt_snapshot(&cache);
    assert_eq!(counts(&hits), vec![("a".to_string(), 2)]);
}

#[test]
fn the_daemons_rule_hits_counter_reveals_lost_events() {
    let (hits, _rx) = handle();
    let rules = synced(&["a"]);
    hits.record(&[ev("a")], 10, 1, &rules);
    hits.record(&[ev("a"), ev("a")], 11, 3, &rules);
    assert!(!view(hits.message()).lossy, "grew by exactly the events");
    hits.record(&[ev("a")], 12, 9, &rules);
    assert!(view(hits.message()).lossy, "five never arrived");
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
    first.record(&[ev("a"), ev("a"), ev("b")], 10, 3, &rules);
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
    first.record(&[ev("a"), ev("gone")], 10, 2, &synced(&["a", "gone"]));
    first.save_now();

    let (second, _rx) = handle();
    second.attach_file(path.clone());
    second.adopt_snapshot(&synced(&["a"]).lock().unwrap());
    assert_eq!(counts(&second), vec![("a".to_string(), 1)]);
    second.record(&[ev("a")], 20, 3, &synced(&["a"]));
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
    first.record(&[ev("a")], 10, 1, &synced(&["a"]));
    first.save_now();

    // A bridge that runs a while with no daemon, saving as it goes.
    let (second, _rx) = handle();
    second.attach_file(path.clone());
    second.record(&[], 1, 0, &synced(&[]));
    second.save_now();
    second.save_now();

    let (third, _rx) = handle();
    third.attach_file(path);
    third.adopt_snapshot(&synced(&["a"]).lock().unwrap());
    assert_eq!(counts(&third), vec![("a".to_string(), 1)]);
}

#[test]
fn a_hit_time_from_the_far_future_still_saves() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, _rx) = handle();
    hits.attach_file(path.clone());
    let far = Event {
        unixnano: (now_ms() + 10 * 86_400_000) * 1_000_000,
        ..ev("a")
    };
    hits.record(&[far], 10, 1, &synced(&["a"]));
    hits.save_now();
    assert!(view(hits.message()).storage.persistent, "the save failed");
    assert!(rule_hits_file::load(&path).unwrap().is_some());
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
    hits.record(&[ev("a")], 10, 1, &rules);
    hits.save_now();
    assert!(path.exists());
    std::fs::remove_file(&path).unwrap();
    hits.save_now();
    assert!(!path.exists(), "nothing changed, so nothing is written");
    hits.record(&[ev("a")], 11, 2, &rules);
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
    hits.record(&[ev("a")], 10, 1, &synced(&["a"]));
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

    hits.record(&[ev("a")], 10, 1, &synced(&["a"]));
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
    hits.record(&[ev("a")], 10, 1, &rules);
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
    hits.record(&[ev("a")], 10, 1, &synced(&["a"]));
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

// N3 (plan `2026-10-09-n3-unused-window-from-daemon-counters.md`): the
// shutdown save, and a bridge restart judged from the daemon's counters.

fn unix_handle() -> (RuleHitsHandle, broadcast::Receiver<ServerMessage>) {
    let (hits, rx) = handle();
    hits.set_daemon_transport(DaemonTransport::Unix);
    (hits, rx)
}

fn stopped_in(path: &std::path::Path) -> Option<i64> {
    rule_hits_file::load(path)
        .unwrap()
        .expect("a saved file")
        .stopped_unix_ms
}

#[test]
fn the_shutdown_save_always_writes_and_says_the_run_stopped_cleanly() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, _rx) = unix_handle();
    hits.attach_file(path.clone());
    hits.record(&[ev("a")], 10, 1, &synced(&["a"]));
    hits.save_now();
    assert_eq!(stopped_in(&path), None, "a periodic save");
    let before = now_ms();
    hits.save_at_stop();
    let stopped = stopped_in(&path).expect("written although nothing changed");
    assert!((before..=now_ms()).contains(&stopped));
    // A periodic save queued before the stop (the ticker's `spawn_blocking`
    // isn't cancelled by aborting the ticker) and run after it writes
    // nothing, changed counts or not.
    hits.record(&[ev("a")], 11, 2, &synced(&["a"]));
    hits.save_now();
    assert_eq!(stopped_in(&path), Some(stopped));
    hits.save_at_stop();
    assert_eq!(stopped_in(&path), Some(stopped), "one stop save");
}

#[test]
fn the_shutdown_save_needs_a_file_and_started_counts() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, _rx) = unix_handle();
    hits.save_at_stop();
    hits.attach_file(path.clone());
    hits.save_at_stop();
    assert!(!path.exists(), "counting hasn't started");
}

/// Run one: a ping with the daemon's counters, then a clean stop or only a
/// periodic save. Run two restores from the same file.
fn second_run(clean: bool, transport: DaemonTransport) -> (RuleHitsHandle, SharedRulesCache) {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let rules = synced(&["a"]);
    let (first, _rx) = unix_handle();
    first.attach_file(path.clone());
    first.record(&[ev("a")], 100, 7, &rules);
    assert!(!view(first.message()).lossy, "the first run has no gap");
    if clean {
        first.save_at_stop();
    } else {
        first.save_now();
    }
    let (second, _rx) = handle();
    second.set_daemon_transport(transport);
    second.attach_file(path);
    second.adopt_snapshot(&rules.lock().unwrap());
    (second, rules)
}

#[test]
fn a_bridge_restart_with_the_daemon_up_is_no_gap_on_the_unix_socket() {
    let (second, rules) = second_run(true, DaemonTransport::Unix);
    assert!(
        view(second.message()).lossy,
        "provisional, until the first ping"
    );
    // Two hits while the bridge was down, delivered with its first ping.
    second.record(&[ev("a"), ev("a")], 101, 9, &rules);
    let v = view(second.message());
    assert!(!v.lossy);
    assert_eq!(v.hits, vec![("a".to_string(), 3)]);
}

#[test]
fn a_bridge_restart_with_a_restarted_daemon_needs_the_clean_stop() {
    let (second, rules) = second_run(true, DaemonTransport::Unix);
    second.record(&[ev("a")], 2, 1, &rules);
    assert!(
        !view(second.message()).lossy,
        "every hit of the new run arrived"
    );
    let (second, rules) = second_run(false, DaemonTransport::Unix);
    second.record(&[ev("a")], 2, 1, &rules);
    assert!(view(second.message()).lossy, "no clean stop: cannot tell");
}

#[test]
fn a_bridge_restart_over_tcp_is_always_a_gap() {
    let (second, rules) = second_run(true, DaemonTransport::Tcp);
    second.record(&[ev("a"), ev("a")], 101, 9, &rules);
    assert!(view(second.message()).lossy);
}

#[test]
fn a_crash_stays_a_crash_through_a_run_that_saw_no_ping() {
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let rules = synced(&["a"]);
    // Run one crashes after a periodic save.
    let (first, _rx) = unix_handle();
    first.attach_file(path.clone());
    first.record(&[ev("a")], 100, 7, &rules);
    first.save_now();
    // Run two: the daemon is idle, no ping; then a clean stop.
    let (second, _rx) = unix_handle();
    second.attach_file(path.clone());
    second.save_at_stop();
    assert_eq!(stopped_in(&path), None, "it vouches for nothing before it");
    // Run three, after a reboot: every hit of the new daemon run arrived,
    // but what run one received after its last save is gone.
    let (third, _rx) = unix_handle();
    third.attach_file(path);
    third.record(&[ev("a")], 2, 1, &rules);
    assert!(view(third.message()).lossy);
}

// Review of PR #123.

fn text_of(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn a_clean_stop_is_consumed_when_read() {
    // Run two judges its first ping and counts, then dies before any save
    // (power loss, SIGKILL at the stop timeout, every save failing). Run
    // three, after a reboot, must not take run one's clean stop for its own.
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let rules = synced(&["a"]);
    let (first, _rx) = unix_handle();
    first.attach_file(path.clone());
    first.record(&[ev("a")], 100, 7, &rules);
    first.save_at_stop();
    assert!(stopped_in(&path).is_some());

    let (second, _rx) = unix_handle();
    second.attach_file(path.clone());
    assert_eq!(stopped_in(&path), None, "rewritten without it at once");
    second.record(&[ev("a"), ev("a")], 101, 9, &rules);
    assert!(!view(second.message()).lossy, "run two judged no gap");
    drop(second);

    let (third, _rx) = unix_handle();
    third.attach_file(path);
    third.record(&[ev("a")], 2, 1, &rules);
    assert!(view(third.message()).lossy, "run two may have lost hits");
}

#[test]
fn a_clean_stop_that_cannot_be_consumed_is_a_gap_now() {
    use std::os::unix::fs::PermissionsExt;
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let rules = synced(&["a"]);
    let (first, _rx) = unix_handle();
    first.attach_file(path.clone());
    first.record(&[ev("a")], 100, 7, &rules);
    first.save_at_stop();
    // Readable, but no temp file can be created next to it.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let (second, _rx) = unix_handle();
    let before = now_ms();
    second.attach_file(path.clone());
    let restored = view(second.message());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(restored.lossy, "the counters aren't trusted");
    assert!(
        !restored.storage.persistent,
        "and the failed write is shown"
    );
    assert!(stopped_in(&path).is_some(), "the file is as it was");
    // A daemon that stayed up and delivered everything changes nothing.
    second.record(&[ev("a"), ev("a")], 101, 9, &rules);
    let gap = match second.message() {
        ServerMessage::RuleHits {
            last_gap_unix_ms, ..
        } => last_gap_unix_ms.unwrap(),
        other => panic!("expected RuleHits, got {other:?}"),
    };
    assert!(gap >= before, "a real gap at the restore, not the old one");
}

#[test]
fn an_untrusting_bridge_writes_the_version_1_shape() {
    // A rollback of the shipped per-user (TCP) bridge must still read it.
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (hits, _rx) = handle();
    hits.attach_file(path.clone());
    hits.record(&[ev("a")], 10, 1, &synced(&["a"]));
    hits.save_now();
    let periodic = text_of(&path);
    hits.save_at_stop();
    for text in [periodic, text_of(&path)] {
        assert!(
            text.starts_with("{\"version\":1,\"sinceUnixMs\":"),
            "{text}"
        );
        assert!(
            !text.contains("daemon") && !text.contains("stopped"),
            "{text}"
        );
    }
}

// Re-review of PR #123.

/// Where the badge's trusted period starts: the later of counting start and
/// the last gap.
fn trusted_from(handle: &RuleHitsHandle) -> i64 {
    match handle.message() {
        ServerMessage::RuleHits {
            since_unix_ms,
            last_gap_unix_ms,
            ..
        } => since_unix_ms.unwrap().max(last_gap_unix_ms.unwrap_or(0)),
        other => panic!("expected RuleHits, got {other:?}"),
    }
}

#[test]
fn a_clean_stop_that_cannot_be_rewritten_is_removed_with_the_file() {
    // Run two can read the file but write nothing for its whole run (a full
    // disk: no temp file fits, but an unlink still works; here a name so
    // long that no temp name fits next to it). It counts hits that are never
    // saved. Run three, storage fixed and the daemon restarted, must not
    // judge from run one's clean stop.
    let dir = state_dir();
    let short = dir.path().join("rule_hits.json");
    let long = dir.path().join(format!("{}.json", "h".repeat(240)));
    let rules = synced(&["a", "b"]);
    let (first, _rx) = unix_handle();
    first.attach_file(short.clone());
    first.record(&[ev("a")], 100, 7, &rules);
    first.save_at_stop();
    std::fs::rename(&short, &long).unwrap();

    let started = now_ms();
    let (second, _rx) = unix_handle();
    second.attach_file(long.clone());
    second.record(&[ev("b"), ev("b")], 1, 2, &rules);
    second.save_now();
    second.save_at_stop();
    assert!(view(second.message()).lossy);
    drop(second);

    if long.exists() {
        std::fs::rename(&long, &short).unwrap();
    }
    let (third, _rx) = unix_handle();
    third.attach_file(short);
    third.record(&[ev("a")], 2, 1, &rules);
    assert!(
        trusted_from(&third) >= started,
        "run two's hits may be missing: no trusted period from before it"
    );
}

#[test]
fn an_untrusting_bridge_rewrites_a_version_2_file_as_version_1_at_once() {
    // A rollback to a bridge from before N3 reads only the version 1 shape.
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    #[allow(dead_code)]
    struct OldReader {
        version: u32,
        since_unix_ms: i64,
        last_gap_unix_ms: Option<i64>,
        hits: Vec<crate::ws_messages::RuleHitWire>,
    }
    let dir = state_dir();
    let path = dir.path().join("rule_hits.json");
    let (unix, _rx) = unix_handle();
    unix.attach_file(path.clone());
    unix.record(&[ev("a")], 100, 7, &synced(&["a"]));
    unix.save_now();
    assert!(text_of(&path).contains("\"daemon\""), "a version 2 file");
    let (tcp, _rx) = handle();
    tcp.attach_file(path.clone());
    let old: OldReader = serde_json::from_str(&text_of(&path)).expect("the old reader");
    assert_eq!(old.version, 1);
    assert!(view(tcp.message()).storage.persistent);
}

#[test]
fn an_untrusting_bridge_that_cannot_rewrite_keeps_the_file() {
    // Only a clean stop that can't be consumed costs the file; over TCP a
    // failed rewrite to version 1 waits for the next good save.
    let dir = state_dir();
    let short = dir.path().join("rule_hits.json");
    let long = dir.path().join(format!("{}.json", "h".repeat(240)));
    let (unix, _rx) = unix_handle();
    unix.attach_file(short.clone());
    unix.record(&[ev("a")], 100, 7, &synced(&["a"]));
    unix.save_now();
    std::fs::rename(&short, &long).unwrap();
    let (tcp, _rx) = handle();
    tcp.attach_file(long.clone());
    assert!(long.exists(), "the counts are not thrown away");
    tcp.adopt_snapshot(&synced(&["a"]).lock().unwrap());
    assert_eq!(counts(&tcp), vec![("a".to_string(), 1)]);
}
