use super::*;
use crate::cache::rule_hits::{DaemonBaseline, MAX_FUTURE_SKEW_MS, MAX_HIT_NAME_BYTES};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn saved(n: usize) -> Saved {
    Saved {
        since_unix_ms: 1_700_000_000_000,
        last_gap_unix_ms: Some(1_700_000_100_000),
        hits: (0..n)
            .map(|i| RuleHitWire {
                name: format!("rule-{i}"),
                count: i as u64 + 1,
                last_hit_unix_ms: 1_700_000_050_000,
            })
            .collect(),
        daemon: Some(DaemonBaseline {
            ping_unix_ms: 1_700_000_060_000,
            uptime: 3_600,
            rule_hits: 42,
        }),
        stopped_unix_ms: Some(1_700_000_070_000),
    }
}

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn invalid(result: io::Result<Option<Saved>>) -> String {
    let err = result.expect_err("should be refused");
    err.to_string()
}

#[test]
fn saved_counts_come_back_unchanged() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    assert_eq!(load(&path).unwrap(), None, "nothing saved yet");
    save(&path, &saved(3)).unwrap();
    assert_eq!(load(&path).unwrap(), Some(saved(3)));
    save(&path, &saved(1)).unwrap();
    assert_eq!(
        load(&path).unwrap(),
        Some(saved(1)),
        "the new file replaced the old"
    );
}

#[test]
fn the_file_is_owner_only_and_leaves_no_temp_file_behind() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    save(&path, &saved(2)).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.mode() & 0o7777, 0o600);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("rule_hits.json")]);
}

#[test]
fn each_save_writes_its_own_temp_file_and_leaves_others_alone() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    assert_ne!(temp_path(&path), temp_path(&path), "unique per save");
    // Another bridge's file (or a crash's leftover) under the old fixed
    // name, and a planted link: neither is removed, followed or reused.
    let leftover = dir.path().join(".rule_hits.json.tmp");
    std::fs::write(&leftover, b"half a write").unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"keep").unwrap();
    symlink(&victim, dir.path().join(".rule_hits.json.link.tmp")).unwrap();
    save(&path, &saved(2)).unwrap();
    assert_eq!(load(&path).unwrap(), Some(saved(2)));
    assert_eq!(std::fs::read(&leftover).unwrap(), b"half a write");
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
}

/// Two bridges on one state directory: neither deletes the other's temp
/// file, so every save succeeds.
#[test]
fn concurrent_saves_to_one_file_all_succeed() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    let savers: Vec<_> = (0..4)
        .map(|n| {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    save(&path, &saved(n + 1)).unwrap();
                }
            })
        })
        .collect();
    for saver in savers {
        saver.join().expect("a save failed");
    }
    assert!(load(&path).unwrap().is_some());
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("rule_hits.json")]);
}

#[test]
fn a_file_with_another_hard_link_is_refused() {
    // Through the other name, a write elsewhere would change what is read
    // here (the same rule as `sqlite_file::file_problem`, #90).
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    save(&path, &saved(1)).unwrap();
    std::fs::hard_link(&path, dir.path().join("elsewhere")).unwrap();
    assert!(invalid(load(&path)).contains("hard link"));
}

#[test]
fn a_link_in_place_of_the_file_is_neither_read_nor_overwritten() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    let other = dir.path().join("other.json");
    save(&other, &saved(2)).unwrap();
    symlink(&other, &path).unwrap();
    assert!(load(&path).is_err(), "read through a link");
    assert!(save(&path, &saved(1)).is_err(), "wrote over a link");
    assert_eq!(load(&other).unwrap(), Some(saved(2)));
}

#[test]
fn something_that_is_not_a_regular_file_is_refused_without_hanging() {
    let dir = dir();
    let fifo = dir.path().join("fifo.json");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(invalid(load(&fifo)).contains("regular file"));
    assert!(save(&fifo, &saved(1)).is_err());

    let sub = dir.path().join("dir.json");
    std::fs::create_dir(&sub).unwrap();
    assert!(load(&sub).is_err());
    assert!(save(&sub, &saved(1)).is_err());
}

#[test]
fn a_file_others_can_write_is_refused_but_a_readable_one_is_fine() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    save(&path, &saved(1)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load(&path).unwrap().is_some());
    for mode in [0o660, 0o606, 0o666] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(invalid(load(&path)).contains("writable"), "mode {mode:o}");
    }
}

#[test]
fn a_file_of_another_user_or_a_special_file_fails_the_ownership_check() {
    let ok = Facts {
        is_file: true,
        uid: 7,
        links: 1,
        mode: 0o600,
        len: 10,
    };
    assert!(check_facts(&ok, 7).is_ok());
    assert!(
        check_facts(&Facts { links: 2, ..ok }, 7).is_err(),
        "another hard link"
    );
    assert!(
        check_facts(&Facts { uid: 8, ..ok }, 7).is_err(),
        "someone else's"
    );
    assert!(check_facts(
        &Facts {
            is_file: false,
            ..ok
        },
        7
    )
    .is_err());
    assert!(check_facts(&Facts { mode: 0o620, ..ok }, 7).is_err());
    assert!(check_facts(&Facts { mode: 0o602, ..ok }, 7).is_err());
    assert!(check_facts(
        &Facts {
            len: MAX_FILE_BYTES + 1,
            ..ok
        },
        7
    )
    .is_err());
    assert!(check_facts(
        &Facts {
            len: MAX_FILE_BYTES,
            ..ok
        },
        7
    )
    .is_ok());
}

#[test]
fn an_oversized_file_is_refused_unread() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    save(&path, &saved(1)).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(invalid(load(&path)).contains("too large"));
}

#[test]
fn what_cannot_be_trusted_is_refused() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    let good = r#"{"version":1,"sinceUnixMs":5,"hits":[{"name":"a","count":1,"lastHitUnixMs":2}]}"#;
    let write = |text: &str| {
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    write(good);
    assert!(load(&path).unwrap().is_some());
    // A clock a little ahead is fine.
    let soon = now() + MAX_FUTURE_SKEW_MS / 2;
    write(&good.replace("\"lastHitUnixMs\":2", &format!("\"lastHitUnixMs\":{soon}")));
    assert!(load(&path).unwrap().is_some());

    let long = "x".repeat(MAX_HIT_NAME_BYTES + 1);
    let many: Vec<String> = (0..=MAX_TRACKED_RULES)
        .map(|i| format!(r#"{{"name":"r{i}","count":1,"lastHitUnixMs":0}}"#))
        .collect();
    let bad = [
        ("not json".to_string(), "parse"),
        (good.replace("\"version\":1", "\"version\":3"), "version"),
        (good.replace("\"version\":1", "\"version\":0"), "version"),
        (
            good.replace(
                "\"hits\"",
                "\"daemon\":{\"pingUnixMs\":-1,\"uptime\":1,\"ruleHits\":1},\"hits\"",
            ),
            "daemon ping",
        ),
        (
            good.replace(
                "\"hits\"",
                &format!(
                    "\"daemon\":{{\"pingUnixMs\":{},\"uptime\":1,\"ruleHits\":1}},\"hits\"",
                    now() + 2 * MAX_FUTURE_SKEW_MS
                ),
            ),
            "daemon ping",
        ),
        (
            good.replace("\"hits\"", "\"daemon\":{\"uptime\":1},\"hits\""),
            "parse",
        ),
        (
            good.replace("\"hits\"", "\"stoppedUnixMs\":-1,\"hits\""),
            "stop time",
        ),
        (
            good.replace(
                "\"hits\"",
                &format!(
                    "\"stoppedUnixMs\":{},\"hits\"",
                    now() + 2 * MAX_FUTURE_SKEW_MS
                ),
            ),
            "stop time",
        ),
        (
            good.replace("\"sinceUnixMs\":5", "\"sinceUnixMs\":-5"),
            "start time",
        ),
        (
            good.replace("\"lastHitUnixMs\":2", "\"lastHitUnixMs\":-1"),
            "last hit",
        ),
        (
            good.replace(
                "\"lastHitUnixMs\":2",
                &format!("\"lastHitUnixMs\":{}", now() + 2 * MAX_FUTURE_SKEW_MS),
            ),
            "last hit",
        ),
        (
            good.replace("\"hits\"", "\"lastGapUnixMs\":-1,\"hits\""),
            "gap",
        ),
        (
            good.replace(
                "\"hits\"",
                &format!(
                    "\"lastGapUnixMs\":{},\"hits\"",
                    now() + 2 * MAX_FUTURE_SKEW_MS
                ),
            ),
            "gap",
        ),
        (
            good.replace(
                "\"sinceUnixMs\":5",
                &format!("\"sinceUnixMs\":{}", now() + 2 * MAX_FUTURE_SKEW_MS),
            ),
            "start time",
        ),
        (good.replace("\"a\"", "\"\""), "name"),
        (good.replace("\"a\"", &format!("\"{long}\"")), "name"),
        (good.replace("\"a\"", "\"a\\u0001\""), "name"),
        (
            good.replace("}]", "},{\"name\":\"a\",\"count\":2,\"lastHitUnixMs\":3}]"),
            "twice",
        ),
        (
            format!(
                r#"{{"version":1,"sinceUnixMs":5,"hits":[{}]}}"#,
                many.join(",")
            ),
            "entries",
        ),
    ];
    for (text, why) in bad {
        write(&text);
        let message = invalid(load(&path));
        assert!(message.contains(why), "{why}: {message}");
    }
}

/// Everything `save` can write is something `load` accepts: otherwise the
/// first restart after a busy week would quietly throw the counts away.
#[test]
fn the_largest_file_save_can_write_loads_back() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    // The worst case for JSON size: quotes and backslashes double.
    let hits = (0..MAX_TRACKED_RULES)
        .map(|i| {
            let stem = format!("{i:05}");
            let name = format!(
                "{stem}{}",
                "\"\\".repeat((MAX_HIT_NAME_BYTES - stem.len()) / 2)
            );
            assert!(name.len() <= MAX_HIT_NAME_BYTES);
            RuleHitWire {
                name,
                count: u64::MAX,
                // As many digits as any valid time has.
                last_hit_unix_ms: now(),
            }
        })
        .collect();
    let biggest = Saved {
        since_unix_ms: now(),
        last_gap_unix_ms: Some(now()),
        hits,
        daemon: Some(DaemonBaseline {
            ping_unix_ms: now(),
            uptime: u64::MAX,
            rule_hits: u64::MAX,
        }),
        stopped_unix_ms: Some(now()),
    };
    save(&path, &biggest).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() <= MAX_FILE_BYTES);
    assert_eq!(load(&path).unwrap(), Some(biggest));
}

#[test]
fn a_save_that_would_not_load_back_is_an_error_not_a_silent_loss() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    let mut too_many = saved(MAX_TRACKED_RULES + 1);
    assert!(save(&path, &too_many).is_err());
    too_many.hits.truncate(1);
    too_many.hits[0].name = String::new();
    assert!(save(&path, &too_many).is_err());
    too_many.hits[0].name = "a".into();
    too_many.hits[0].last_hit_unix_ms = now() + 2 * MAX_FUTURE_SKEW_MS;
    assert!(save(&path, &too_many).is_err(), "a time load would refuse");
    assert!(!path.exists());
}

/// N3: what the next run judges a restart from, written as version 2.
#[test]
fn the_daemon_baseline_and_a_clean_stop_are_saved_as_version_2() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    save(&path, &saved(1)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"version\":2"), "{text}");
    assert!(
        text.contains("\"daemon\":{\"pingUnixMs\":1700000060000,\"uptime\":3600,\"ruleHits\":42}"),
        "{text}"
    );
    assert!(text.contains("\"stoppedUnixMs\":1700000070000"), "{text}");
    let none = Saved {
        daemon: None,
        stopped_unix_ms: None,
        ..saved(1)
    };
    save(&path, &none).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("daemon") && !text.contains("stopped"),
        "{text}"
    );
    assert_eq!(load(&path).unwrap(), Some(none));
}

/// A file an older bridge wrote still loads, with nothing to judge a
/// restart from.
#[test]
fn a_version_1_file_loads_without_a_baseline() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    std::fs::write(
        &path,
        r#"{"version":1,"sinceUnixMs":5,"lastGapUnixMs":6,"hits":[{"name":"a","count":1,"lastHitUnixMs":2}]}"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = load(&path).unwrap().expect("a version 1 file loads");
    assert_eq!(loaded.since_unix_ms, 5);
    assert_eq!(loaded.last_gap_unix_ms, Some(6));
    assert_eq!(loaded.hits.len(), 1);
    assert_eq!(loaded.daemon, None);
    assert_eq!(loaded.stopped_unix_ms, None);
}

/// A field this bridge doesn't know (a later additive change) is ignored,
/// not a reason to distrust the whole file.
#[test]
fn an_unknown_field_is_ignored() {
    let dir = dir();
    let path = dir.path().join("rule_hits.json");
    std::fs::write(
        &path,
        r#"{"version":2,"sinceUnixMs":5,"later":{"x":[1]},"hits":[{"name":"a","count":1,"lastHitUnixMs":2,"more":true}],"daemon":{"pingUnixMs":7,"uptime":1,"ruleHits":3,"bootId":"x"}}"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = load(&path).unwrap().expect("unknown fields are ignored");
    assert_eq!(
        loaded.daemon,
        Some(DaemonBaseline {
            ping_unix_ms: 7,
            uptime: 1,
            rule_hits: 3,
        })
    );
    assert_eq!(loaded.hits[0].name, "a");
}
