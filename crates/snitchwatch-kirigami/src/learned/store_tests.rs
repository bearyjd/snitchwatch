//! `learned.json` against real files in temporary directories.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;

use super::*;
use crate::bridge_runtime::PendingRow;
use crate::learned::aggregate::Program;
use crate::learned::path_key;

const NOW: i64 = 10_000_000_000;

fn row(path: &str, host: &str) -> PendingRow {
    PendingRow {
        process: "p".into(),
        process_path: Some(path.into()),
        dst_host: host.into(),
        dst_ip: "93.184.216.34".into(),
        port: 443,
        protocol: "tcp".into(),
    }
}

/// `n` connections of `path` to `host`, first seen at `at`.
fn delta(path: &str, host: &str, n: u64, at: i64) -> Aggregate {
    let mut agg = Aggregate::default();
    for _ in 0..n {
        agg.absorb(Program::observed(&row(path, host), at));
    }
    agg
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self) -> PathBuf {
        self.dir.path().join("cfg").join("learned.json")
    }
    fn backend(&self) -> FileBackend {
        FileBackend::new(self.path())
    }
    /// Quick locks: a contended test must not wait long.
    fn backend_with(&self, attempts: u32, pause_ms: u64) -> FileBackend {
        FileBackend::with_policy(
            self.path(),
            LockPolicy {
                attempts,
                pause: Duration::from_millis(pause_ms),
            },
        )
    }
    fn stored(&self) -> Aggregate {
        self.backend().load(NOW).aggregate
    }
    fn plant(&self, body: &str) {
        std::fs::create_dir_all(self.path().parent().unwrap()).unwrap();
        std::fs::write(self.path(), body).unwrap();
    }
}

fn connections(agg: &Aggregate, key: &str) -> u64 {
    agg.c.get(&path_key(key)).map_or(0, |p| p.connections)
}

// --- reading and writing ---------------------------------------------------------

#[test]
fn a_missing_file_loads_empty_without_a_notice() {
    let fx = Fixture::new();
    let loaded = fx.backend().load(NOW);
    assert!(loaded.aggregate.is_empty());
    assert_eq!(loaded.notice, None);
}

#[test]
fn a_write_is_private_atomic_and_reads_back() {
    let fx = Fixture::new();
    let report = fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 2, NOW), NOW)
        .unwrap();
    assert_eq!(report, WriteReport::default());
    let meta = std::fs::metadata(fx.path()).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.uid(), unsafe { libc::geteuid() });
    // No temporary file is left beside it (the rename replaced the target).
    let names: Vec<String> = std::fs::read_dir(fx.path().parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains(".tmp-")),
        "left over: {names:?}"
    );
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 2);
}

#[test]
fn deltas_are_summed_into_what_is_stored() {
    let fx = Fixture::new();
    let backend = fx.backend();
    backend
        .merge_write(&delta("/usr/bin/curl", "example.com", 2, NOW), NOW)
        .unwrap();
    backend
        .merge_write(&delta("/usr/bin/curl", "other.example", 3, NOW + 5), NOW)
        .unwrap();
    let stored = fx.stored();
    let curl = &stored.c[&path_key("/usr/bin/curl")];
    assert_eq!(curl.connections, 5);
    assert_eq!(curl.destinations["example.com"], 2);
    assert_eq!(curl.destinations["other.example"], 3);
    assert_eq!((curl.first_seen_ms, curl.last_seen_ms), (NOW, NOW + 5));
}

#[test]
fn what_another_copy_wrote_in_between_is_kept() {
    let fx = Fixture::new();
    let mine = fx.backend();
    let other = fx.backend();
    mine.merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    other
        .merge_write(&delta("/usr/bin/wget", "example.com", 4, NOW), NOW)
        .unwrap();
    mine.merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    let stored = fx.stored();
    assert_eq!(connections(&stored, "/usr/bin/curl"), 2);
    assert_eq!(connections(&stored, "/usr/bin/wget"), 4);
}

#[test]
fn clear_deletes_the_file_and_a_missing_one_is_fine() {
    let fx = Fixture::new();
    fx.backend().clear().unwrap();
    fx.backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    assert!(fx.path().exists());
    fx.backend().clear().unwrap();
    assert!(!fx.path().exists());
    assert!(fx.stored().is_empty());
}

// --- a file that cannot be trusted ----------------------------------------------

fn aside_of(fx: &Fixture) -> PathBuf {
    let mut name = fx.path().file_name().unwrap().to_os_string();
    name.push(".corrupt");
    fx.path().with_file_name(name)
}

#[test]
fn a_file_that_cannot_be_used_is_kept_aside_never_destroyed() {
    // Damaged, an older format, and valid JSON of the wrong shape.
    for body in [
        "",
        "{",
        "not json",
        "[1,2,3]",
        r#"{"version":1,"saved_ms":0,"aggregate":{}}"#,
        r#"{"version":2,"saved_ms":"soon"}"#,
    ] {
        let fx = Fixture::new();
        fx.plant(body);
        let loaded = fx.backend().load(NOW);
        assert!(loaded.aggregate.is_empty(), "{body}");
        assert_eq!(loaded.notice, Some(Notice::Corrupt), "{body}");
        assert_eq!(
            std::fs::read_to_string(fx.path()).unwrap(),
            body,
            "a read changes nothing"
        );

        let report = fx
            .backend()
            .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
            .unwrap();
        assert_eq!(report.notice, Some(Notice::Corrupt), "{body}");
        assert_eq!(
            std::fs::read_to_string(aside_of(&fx)).unwrap(),
            body,
            "kept whole: {body}"
        );
        assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 1, "{body}");
        assert_eq!(fx.backend().load(NOW).notice, None, "{body}");
    }
}

#[test]
fn a_file_from_a_newer_version_is_never_overwritten() {
    let fx = Fixture::new();
    let newer = r#"{"version":99,"saved_ms":1,"aggregate":{"future":"data"},"extra":[1,2,3]}"#;
    fx.plant(newer);
    let loaded = fx.backend().load(NOW);
    assert!(loaded.aggregate.is_empty());
    assert_eq!(loaded.notice, Some(Notice::Newer));
    let report = fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    assert_eq!(report.notice, Some(Notice::Newer));
    assert_eq!(
        std::fs::read_to_string(aside_of(&fx)).unwrap(),
        newer,
        "byte for byte"
    );
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 1);
    assert!(Notice::Newer.text().contains("newer"));
}

#[test]
fn an_oversized_file_is_not_read_whole_and_is_kept_aside() {
    let fx = Fixture::new();
    // Valid JSON, so only the size can reject it.
    let padded = format!(
        r#"{{"version":2,"saved_ms":0,"aggregate":{{}}}}{}"#,
        " ".repeat(20_000)
    );
    fx.plant(&padded);
    let small = || fx.backend().with_max_bytes(10_000);
    assert_eq!(small().load(NOW).notice, Some(Notice::Corrupt));
    small()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    assert_eq!(std::fs::read_to_string(aside_of(&fx)).unwrap(), padded);
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 1);
}

#[test]
fn a_newer_aside_replaces_only_an_older_one() {
    let fx = Fixture::new();
    fx.plant("first damaged file");
    fx.backend()
        .merge_write(&delta("/usr/bin/a", "example.com", 1, NOW), NOW)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(aside_of(&fx)).unwrap(),
        "first damaged file"
    );
    fx.plant("second damaged file");
    fx.backend()
        .merge_write(&delta("/usr/bin/a", "example.com", 1, NOW), NOW)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(aside_of(&fx)).unwrap(),
        "second damaged file"
    );
}

#[test]
fn nothing_is_written_when_the_file_cannot_be_moved_aside() {
    let fx = Fixture::new();
    fx.plant("damaged");
    // A directory with something in it where the aside belongs: the rename fails.
    std::fs::create_dir_all(aside_of(&fx).join("inner")).unwrap();
    let err = fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap_err();
    assert_eq!(err, Notice::WriteFailed);
    assert_eq!(
        std::fs::read_to_string(fx.path()).unwrap(),
        "damaged",
        "untouched"
    );
}

#[test]
fn a_hostile_ring_entry_in_the_file_is_cut_and_made_safe_on_load() {
    let fx = Fixture::new();
    let mut agg = Aggregate::default();
    agg.ring.push_back(crate::learned::aggregate::Dropped {
        display: format!("<img src=x>\u{202e}\n{}", "y".repeat(50_000)),
        kind: crate::learned::aggregate::DropKind::Evicted,
        class: crate::learned::aggregate::Class::C,
        connections: 1,
        flagged: false,
    });
    agg.created_ms = NOW;
    let body = FileBody {
        version: VERSION,
        saved_ms: NOW,
        aggregate: agg,
    };
    fx.plant(&serde_json::to_string(&body).unwrap());
    let loaded = fx.backend().load(NOW);
    assert_eq!(loaded.notice, None);
    let shown = &loaded.aggregate.ring[0].display;
    assert!(shown.len() <= 256, "{}", shown.len());
    assert!(!shown.chars().any(char::is_control) && !shown.contains('\u{202e}'));
}

/// The worst case the recorder can produce is accepted back: every class
/// filled with maximal entries, written, and read again.
#[test]
fn the_largest_file_the_recorder_can_write_is_accepted_and_within_the_budget() {
    use crate::learned::aggregate::{BYTE_BUDGET, FILE_OVERHEAD};
    let fx = Fixture::new();
    let mut agg = Aggregate::default();
    for n in 0..700u64 {
        let class_dirs = [
            ("/opt/", true, true),
            ("/tmp/", false, true),
            ("/usr/bin/", false, true),
            ("", false, false),
        ];
        for (i, (dir, flagged, has_path)) in class_dirs.into_iter().enumerate() {
            let path = format!("{dir}{i}-{n:05}{}", "p".repeat(2_000));
            let mut r = row(&path, "h.example");
            r.process = format!("{n}{}", "n".repeat(400));
            if !has_path {
                r.process_path = None;
            }
            let mut p = Program::observed(&r, NOW + n as i64);
            p.destinations.clear();
            for d in 0..80 {
                p.destinations
                    .insert(format!("{d:03}{}", "h\"".repeat(200)), 1 + d);
            }
            p.ports.extend(1000..1100);
            p.uncommon_port = u64::from(flagged);
            agg.absorb(p);
        }
    }
    assert!(
        agg.programs_seen() > 100,
        "the budget was reached with real content"
    );
    assert!(agg.dropped() > 0, "and it held");
    assert!(agg.bytes <= BYTE_BUDGET);
    let json_len = serde_json::to_vec(&agg).unwrap().len();
    assert!(json_len <= BYTE_BUDGET + FILE_OVERHEAD, "{json_len}");

    fx.backend().merge_write(&agg, NOW).unwrap();
    let on_disk = std::fs::metadata(fx.path()).unwrap().len();
    assert!(on_disk as usize <= BYTE_BUDGET + FILE_OVERHEAD, "{on_disk}");
    assert!(
        on_disk < MAX_FILE_BYTES / 2,
        "a wide margin below the limit: {on_disk}"
    );
    let loaded = fx.backend().load(NOW);
    assert_eq!(
        loaded.notice, None,
        "the recorder's own file is never 'corrupt'"
    );
    assert_eq!(loaded.aggregate.programs_seen(), agg.programs_seen());
    assert!(!aside_of(&fx).exists());
}

#[test]
fn a_symlink_a_hard_link_and_a_fifo_are_not_ours_to_use_or_change() {
    // Symlink to a real file.
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.path().parent().unwrap()).unwrap();
    let target = fx.dir.path().join("target.json");
    std::fs::write(&target, "{}").unwrap();
    std::os::unix::fs::symlink(&target, fx.path()).unwrap();
    let loaded = fx.backend().load(NOW);
    assert_eq!(
        (loaded.aggregate.is_empty(), loaded.notice),
        (true, Some(Notice::Untrusted))
    );
    let err = fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap_err();
    assert_eq!(err, Notice::Untrusted);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "{}",
        "target untouched"
    );

    // A file with a second name.
    let fx = Fixture::new();
    fx.backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    let link = fx.dir.path().join("second-name");
    std::fs::hard_link(fx.path(), &link).unwrap();
    assert_eq!(fx.backend().load(NOW).notice, Some(Notice::Untrusted));
    assert_eq!(
        fx.backend()
            .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
            .unwrap_err(),
        Notice::Untrusted
    );

    // A FIFO: never waited on.
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.path().parent().unwrap()).unwrap();
    let c = std::ffi::CString::new(fx.path().to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(fx.backend().load(NOW).notice, Some(Notice::Untrusted));
}

#[test]
fn a_file_of_another_owner_is_untrusted_when_we_can_make_one() {
    // Only root can give a file away; elsewhere the hard-link and symlink
    // cases above cover the same refusal.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let fx = Fixture::new();
    fx.plant("{}");
    let c = std::ffi::CString::new(fx.path().to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::chown(c.as_ptr(), 65534, u32::MAX) }, 0);
    assert_eq!(fx.backend().load(NOW).notice, Some(Notice::Untrusted));
}

#[test]
fn an_unreadable_path_is_reported_not_trusted() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.path()).unwrap(); // a directory where the file belongs
    assert!(fx.backend().load(NOW).notice.is_some());
    assert!(fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .is_err());
}

// --- retention ---------------------------------------------------------------------

#[test]
fn a_file_older_than_thirty_days_is_discarded_on_load_and_replaced_on_write() {
    let fx = Fixture::new();
    let old = NOW - RETENTION_MS - 1;
    fx.backend()
        .merge_write(&delta("/usr/bin/old", "example.com", 3, old), old)
        .unwrap();
    let loaded = fx.backend().load(NOW);
    assert!(loaded.aggregate.is_empty());
    assert_eq!(loaded.notice, Some(Notice::Expired));

    fx.backend()
        .merge_write(&delta("/usr/bin/new", "example.com", 1, NOW), NOW)
        .unwrap();
    let stored = fx.stored();
    assert_eq!(
        connections(&stored, "/usr/bin/old"),
        0,
        "the old list is gone"
    );
    assert_eq!(connections(&stored, "/usr/bin/new"), 1);
}

#[test]
fn a_file_just_inside_thirty_days_is_kept() {
    let fx = Fixture::new();
    let edge = NOW - RETENTION_MS + 1_000;
    fx.backend()
        .merge_write(&delta("/usr/bin/old", "example.com", 1, edge), edge)
        .unwrap();
    let loaded = fx.backend().load(NOW);
    assert_eq!(loaded.notice, None);
    assert_eq!(connections(&loaded.aggregate, "/usr/bin/old"), 1);
}

#[test]
fn the_age_is_when_it_was_first_written_not_when_it_was_last_touched() {
    // Every write touches the file; its age must still run out.
    let fx = Fixture::new();
    let start = NOW - RETENTION_MS - 1_000;
    let backend = fx.backend();
    backend
        .merge_write(&delta("/usr/bin/a", "example.com", 1, start), start)
        .unwrap();
    backend
        .merge_write(
            &delta("/usr/bin/a", "example.com", 1, NOW - 1_000),
            NOW - 1_000,
        )
        .unwrap();
    assert_eq!(backend.load(NOW).notice, Some(Notice::Expired));
}

#[test]
fn the_startup_housekeeping_deletes_only_a_stale_file() {
    let fx = Fixture::new();
    assert!(!discard_if_stale(&fx.path(), NOW), "nothing to delete");
    let old = NOW - RETENTION_MS - 1;
    fx.backend()
        .merge_write(&delta("/usr/bin/old", "example.com", 1, old), old)
        .unwrap();
    assert!(discard_if_stale(&fx.path(), NOW));
    assert!(!fx.path().exists());
    fx.backend()
        .merge_write(&delta("/usr/bin/new", "example.com", 1, NOW), NOW)
        .unwrap();
    assert!(!discard_if_stale(&fx.path(), NOW));
    assert!(fx.path().exists());
    // A file that cannot be understood is left for the owner to look at.
    fx.plant("not json");
    assert!(!discard_if_stale(&fx.path(), NOW));
    assert!(fx.path().exists());
}

// --- the advisory lock ------------------------------------------------------------------

/// Take the lock the way another copy of the program would: its own open file
/// description (flock conflicts per description, even within one process).
fn hold_lock(path: &Path) -> std::fs::File {
    use std::os::unix::io::AsRawFd;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path_for(path))
        .unwrap();
    // SAFETY: a valid, open file descriptor.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}

#[test]
fn the_lock_is_a_separate_private_file_and_a_free_lock_is_not_a_degrade() {
    let fx = Fixture::new();
    let report = fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    assert!(!report.lock_degraded);
    let lock = lock_path_for(&fx.path());
    assert_eq!(lock.file_name().unwrap(), "learned.json.lock");
    assert_eq!(
        std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_lock_held_for_good_degrades_to_last_writer_wins_and_says_so() {
    let fx = Fixture::new();
    let _held = hold_lock(&fx.path());
    let report = fx
        .backend_with(3, 5)
        .merge_write(&delta("/usr/bin/curl", "example.com", 2, NOW), NOW)
        .unwrap();
    assert!(
        report.lock_degraded,
        "the write went ahead without the lock"
    );
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 2);
}

#[test]
fn a_lock_released_while_waiting_is_taken_without_degrading() {
    let fx = Fixture::new();
    let held = hold_lock(&fx.path());
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        drop(held);
    });
    let report = fx
        .backend_with(200, 5)
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    releaser.join().unwrap();
    assert!(!report.lock_degraded);
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 1);
}

#[test]
fn a_lock_file_that_cannot_be_opened_degrades_and_still_writes() {
    let fx = Fixture::new();
    std::fs::create_dir_all(lock_path_for(&fx.path())).unwrap(); // a directory in its place
    let report = fx
        .backend()
        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
        .unwrap();
    assert!(report.lock_degraded);
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 1);
}

#[test]
fn two_writers_under_the_lock_lose_no_update() {
    let fx = Fixture::new();
    let path = fx.path();
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let backend = Arc::new(FileBackend::with_policy(
                path.clone(),
                LockPolicy {
                    attempts: 2_000,
                    pause: Duration::from_millis(2),
                },
            ));
            std::thread::spawn(move || {
                for _ in 0..15 {
                    let report = backend
                        .merge_write(&delta("/usr/bin/curl", "example.com", 1, NOW), NOW)
                        .unwrap();
                    assert!(!report.lock_degraded);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(connections(&fx.stored(), "/usr/bin/curl"), 30);
}
