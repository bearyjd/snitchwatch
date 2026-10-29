use super::*;
use std::os::unix::fs::PermissionsExt;

const T0: i64 = 1_800_000_000_000;

fn uid() -> u32 {
    crate::logging::effective_uid()
}

fn dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("snitchwatch");
    (tmp, root.join(SEEN_FILE_NAME))
}

fn seen(state: &SeenPrograms) -> Vec<&str> {
    let mut paths: Vec<&str> = state.seen_paths().collect();
    paths.sort_unstable();
    paths
}

fn baselined(path: &Path, present: &[&str]) -> SeenPrograms {
    let mut state = SeenPrograms::persisted(path.to_path_buf(), uid());
    state.note_clear_applied();
    assert!(state.on_rules_applied(present.iter().copied(), T0));
    assert!(state.sync(), "the baseline was not written");
    state
}

#[test]
fn a_missing_file_baselines_the_present_programs_with_the_notice() {
    let (_tmp, path) = dir();
    let state = baselined(&path, &["/usr/bin/a", "/usr/bin/b"]);
    assert_eq!(seen(&state), ["/usr/bin/a", "/usr/bin/b"]);
    assert!(
        state.notice().starts_with("Baseline recorded"),
        "{}",
        state.notice()
    );
    assert_eq!(state.new_count(), 0);
}

#[test]
fn the_baseline_is_written_owner_only_and_read_back_without_a_notice() {
    let (_tmp, path) = dir();
    baselined(&path, &["/usr/bin/a"]);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);

    let again = baselined(&path, &["/usr/bin/a"]);
    assert_eq!(seen(&again), ["/usr/bin/a"]);
    assert_eq!(again.notice(), "", "a valid file is no re-baseline");
}

#[test]
fn with_a_valid_file_a_program_running_at_the_baseline_is_new() {
    let (_tmp, path) = dir();
    baselined(&path, &["/usr/bin/a"]);
    let again = baselined(&path, &["/usr/bin/a", "/tmp/evil"]);
    assert!(again.is_new("/tmp/evil"));
    assert_eq!(seen(&again), ["/usr/bin/a"]);
}

#[test]
fn a_corrupt_file_re_baselines_with_the_notice_every_time() {
    let (_tmp, path) = dir();
    crate::private_files::ensure_private_dir(path.parent().unwrap()).unwrap();
    for body in [
        "not json",
        "{\"version\":99,\"programs\":[]}",
        "{\"version\":1}",
    ] {
        crate::private_files::write_private(&path, body.as_bytes()).unwrap();
        assert_eq!(read(&path, uid()), Loaded::Corrupt, "{body}");
        let state = baselined(&path, &["/usr/bin/a"]);
        assert!(state.notice().contains("damaged"), "{}", state.notice());
        assert!(state.notice().contains("Baseline recorded"));
        assert_eq!(seen(&state), ["/usr/bin/a"]);
        // The write replaced the damaged file.
        assert!(matches!(read(&path, uid()), Loaded::Valid(_)));
    }
}

#[test]
fn a_file_owned_by_someone_else_re_baselines_with_the_notice() {
    let (_tmp, path) = dir();
    baselined(&path, &["/usr/bin/a", "/tmp/planted"]);
    let other = uid().wrapping_add(1);
    assert_eq!(read(&path, other), Loaded::Foreign);
    let mut state = SeenPrograms::persisted(path.clone(), other);
    assert_eq!(
        seen(&state),
        Vec::<&str>::new(),
        "nothing is trusted from it"
    );
    state.note_clear_applied();
    assert!(state.on_rules_applied(["/usr/bin/a"], T0));
    assert!(
        state.notice().contains("not a plain file of yours"),
        "{}",
        state.notice()
    );
    assert_eq!(seen(&state), ["/usr/bin/a"]);
}

#[test]
fn a_symlink_or_hard_link_in_place_of_the_file_is_foreign() {
    let (tmp, path) = dir();
    baselined(&path, &["/usr/bin/a"]);
    let linked = tmp.path().join("linked.json");
    std::fs::hard_link(&path, &linked).unwrap();
    assert_eq!(read(&path, uid()), Loaded::Foreign);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&linked, &path).unwrap();
    assert_eq!(read(&path, uid()), Loaded::Foreign);
}

#[test]
fn nothing_is_baselined_before_a_clear_has_been_applied() {
    let mut state = SeenPrograms::default();
    assert!(!state.on_rules_applied(["/usr/bin/a"], T0));
    assert!(!state.is_baselined());
    assert!(
        !state.observe(["/usr/bin/a"], T0),
        "no flags before the baseline"
    );
    assert_eq!(state.new_count(), 0);
    assert_eq!(state.notice(), "");
}

#[test]
fn a_program_appearing_after_the_baseline_is_new_until_acknowledged() {
    let mut state = SeenPrograms::default();
    state.note_clear_applied();
    assert!(state.on_rules_applied(["/usr/bin/a"], T0));
    assert!(state.observe(["/usr/bin/a", "/usr/bin/late"], T0 + 1));
    assert!(state.is_new("/usr/bin/late"));
    assert_eq!(seen(&state), ["/usr/bin/a"]);
    assert!(!state.observe(["/usr/bin/late"], T0 + 2), "flagged once");
    assert!(state.acknowledge(T0 + 3));
    assert!(!state.is_new("/usr/bin/late"));
    assert_eq!(seen(&state), ["/usr/bin/a", "/usr/bin/late"]);
    assert!(!state.acknowledge(T0 + 4));
}

#[test]
fn a_reconnect_resets_neither_the_flag_nor_the_set() {
    let mut state = SeenPrograms::default();
    state.note_clear_applied();
    state.on_rules_applied(["/usr/bin/a"], T0);
    state.observe(["/usr/bin/late"], T0);
    // The next session's snapshot: clear, replay, rules.
    state.note_clear_applied();
    assert!(!state.on_rules_applied(["/usr/bin/a", "/usr/bin/late", "/x"], T0 + 9));
    assert!(state.is_new("/usr/bin/late"), "still new");
    assert!(
        !state.is_new("/x") && !seen(&state).contains(&"/x"),
        "not re-baselined"
    );
    assert_eq!(seen(&state), ["/usr/bin/a"]);
}

#[test]
fn an_unacknowledged_program_is_never_persisted() {
    let (_tmp, path) = dir();
    let mut state = baselined(&path, &["/usr/bin/a"]);
    state.observe(["/usr/bin/late"], T0);
    drop(state);
    let again = baselined(&path, &["/usr/bin/late"]);
    assert!(again.is_new("/usr/bin/late"));
}

#[test]
fn an_acknowledged_program_is_persisted() {
    let (_tmp, path) = dir();
    let mut state = baselined(&path, &["/usr/bin/a"]);
    state.observe(["/usr/bin/late"], T0);
    state.acknowledge(T0 + 1);
    assert!(state.sync());
    let again = baselined(&path, &["/usr/bin/late"]);
    assert!(!again.is_new("/usr/bin/late"));
}

#[test]
fn writes_merge_with_another_gui_instead_of_overwriting_it() {
    let (_tmp, path) = dir();
    let mut first = baselined(&path, &["/usr/bin/a"]);
    let mut second = SeenPrograms::persisted(path.clone(), uid());
    second.note_clear_applied();
    second.on_rules_applied(["/usr/bin/b"], T0);
    second.observe(["/usr/bin/c"], T0);
    second.acknowledge(T0);
    first.observe(["/usr/bin/d"], T0);
    first.acknowledge(T0);
    assert!(second.sync() && first.sync());
    let Loaded::Valid(on_disk) = read(&path, uid()) else {
        panic!("unreadable")
    };
    let mut paths: Vec<&str> = on_disk.keys().map(String::as_str).collect();
    paths.sort_unstable();
    // `b` was running at the second GUI's baseline over a valid file, so it
    // was new there until that GUI acknowledged it with `c`.
    assert_eq!(
        paths,
        ["/usr/bin/a", "/usr/bin/b", "/usr/bin/c", "/usr/bin/d"]
    );
}

#[test]
fn the_set_is_capped_and_the_least_recently_seen_go_first() {
    let (_tmp, path) = dir();
    let mut ours: HashMap<String, i64> = (0..MAX_SEEN as i64 + 10)
        .map(|i| (format!("/usr/bin/p{i}"), T0 + i))
        .collect();
    ours.insert("/usr/bin/p0".to_string(), T0 + 1_000_000);
    let merged = write_merged(&path, &ours, uid()).unwrap();
    assert_eq!(merged.len(), MAX_SEEN);
    assert!(merged.contains_key("/usr/bin/p0"), "seen recently, kept");
    for i in 1..=10 {
        assert!(
            !merged.contains_key(&format!("/usr/bin/p{i}")),
            "p{i} evicted"
        );
    }
    let Loaded::Valid(back) = read(&path, uid()) else {
        panic!("unreadable")
    };
    assert_eq!(back, merged);
}

/// Issue #240: 5,000 paths of ~1 KiB serialize past the reader's limit.
/// The writer trims the least recently seen to fit the same budget, so the
/// file it writes always reads back as valid (no rebaseline next launch).
#[test]
fn a_full_set_of_long_paths_fits_the_read_budget_and_reads_back() {
    let (_tmp, path) = dir();
    let long = "x".repeat(1_000);
    let ours: HashMap<String, i64> = (0..MAX_SEEN as i64)
        .map(|i| (format!("/opt/{long}/p{i:04}"), T0 + i))
        .collect();
    let merged = write_merged(&path, &ours, uid()).unwrap();
    let written = std::fs::metadata(&path).unwrap().len();
    assert!(written <= MAX_BYTES, "{written} bytes written");
    assert!(merged.len() < MAX_SEEN, "nothing was trimmed");
    assert!(
        merged.len() > MAX_SEEN * 3 / 4,
        "trimmed far more than needed: {}",
        merged.len()
    );
    // The newest are kept, the oldest go.
    assert!(merged.contains_key(&format!("/opt/{long}/p{:04}", MAX_SEEN - 1)));
    assert!(!merged.contains_key(&format!("/opt/{long}/p0000")));
    let oldest_kept = merged.values().min().copied().unwrap();
    assert!(ours
        .iter()
        .filter(|(_, &t)| t >= oldest_kept)
        .all(|(p, _)| merged.contains_key(p)));
    let Loaded::Valid(back) = read(&path, uid()) else {
        panic!("the GUI's own file was rejected on read")
    };
    assert_eq!(back, merged);
}

#[test]
fn a_set_within_the_budget_is_written_whole() {
    let (_tmp, path) = dir();
    let ours: HashMap<String, i64> = (0..MAX_SEEN as i64)
        .map(|i| (format!("/usr/bin/p{i}"), T0 + i))
        .collect();
    let merged = write_merged(&path, &ours, uid()).unwrap();
    assert_eq!(merged.len(), MAX_SEEN);
}

#[test]
fn a_later_sighting_moves_a_path_to_the_front_of_the_lru() {
    let mut state = SeenPrograms::default();
    state.note_clear_applied();
    state.on_rules_applied(["/usr/bin/a"], T0);
    state.observe(["/usr/bin/a"], T0 + 50);
    assert_eq!(state.seen.get("/usr/bin/a"), Some(&(T0 + 50)));
}

#[test]
fn dismissing_the_notice_clears_it() {
    let mut state = SeenPrograms::default();
    state.note_clear_applied();
    state.on_rules_applied([], T0);
    assert!(
        !state.notice().is_empty(),
        "an empty snapshot still baselines"
    );
    state.dismiss_notice();
    assert_eq!(state.notice(), "");
}

#[test]
fn a_file_others_may_write_or_a_shared_directory_re_baselines() {
    let (_tmp, path) = dir();
    baselined(&path, &["/usr/bin/a"]);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o620)).unwrap();
    assert_eq!(read(&path, uid()), Loaded::Foreign, "group-writable file");
    let state = baselined(&path, &["/usr/bin/b"]);
    assert!(
        state.notice().contains("not a plain file of yours"),
        "{}",
        state.notice()
    );
    assert_eq!(seen(&state), ["/usr/bin/b"]);

    // The rewrite tightened it; now open the directory instead.
    assert!(matches!(read(&path, uid()), Loaded::Valid(_)));
    let parent = path.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        read(&path, uid()),
        Loaded::Foreign,
        "world-readable directory"
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(read(&path, uid()), Loaded::Valid(_)));
}

#[test]
fn a_write_waits_for_another_writer_holding_the_lock() {
    let (_tmp, path) = dir();
    baselined(&path, &["/usr/bin/a"]);
    let lock_path = crate::learned::lock_path_for(&path);
    assert!(lock_path.ends_with("seen-programs.json.lock"));
    let (held, degraded) =
        crate::learned::acquire_lock(&lock_path, &crate::learned::LockPolicy::default());
    assert!(held.is_some() && !degraded);

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        let ours = HashMap::from([("/usr/bin/w".to_string(), T0)]);
        let merged = write_merged(&writer_path, &ours, uid()).unwrap();
        done_tx.send(()).unwrap();
        merged
    });
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(
        done_rx.try_recv().is_err(),
        "the write did not wait for the lock"
    );
    // The other GUI's write lands while it holds the lock ...
    let other = HashMap::from([("/usr/bin/other".to_string(), T0)]);
    let body = serde_json::to_vec(&FileBody {
        version: VERSION,
        programs: capped(other.into_iter().chain([("/usr/bin/a".to_string(), T0)]))
            .map(|(path, last_seen_ms)| Entry { path, last_seen_ms })
            .collect(),
    })
    .unwrap();
    crate::private_files::write_private(&path, &body).unwrap();
    drop(held);
    // ... and the waiting write reads it before replacing the file.
    let merged = writer.join().unwrap();
    let mut paths: Vec<&str> = merged.keys().map(String::as_str).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["/usr/bin/a", "/usr/bin/other", "/usr/bin/w"]);
}

#[test]
fn last_seen_times_persist_debounced_and_on_drop() {
    let (_tmp, path) = dir();
    let mut state = baselined(&path, &["/usr/bin/a"]);
    let on_disk = |path: &Path| match read(path, uid()) {
        Loaded::Valid(seen) => seen.get("/usr/bin/a").copied(),
        other => panic!("{other:?}"),
    };
    state.observe(["/usr/bin/a"], T0 + 10);
    assert_eq!(on_disk(&path), Some(T0), "saved again within the debounce");
    state.observe(["/usr/bin/a"], T0 + TOUCH_PERSIST_MS);
    assert!(state.sync());
    assert_eq!(
        on_disk(&path),
        Some(T0 + TOUCH_PERSIST_MS),
        "not saved after the debounce"
    );
    state.observe(["/usr/bin/a", "/usr/bin/new"], T0 + TOUCH_PERSIST_MS + 5);
    drop(state);
    assert_eq!(
        on_disk(&path),
        Some(T0 + TOUCH_PERSIST_MS + 5),
        "not saved on drop"
    );
    let Loaded::Valid(saved) = read(&path, uid()) else {
        panic!("unreadable")
    };
    assert!(
        !saved.contains_key("/usr/bin/new"),
        "an unacknowledged program was saved"
    );
}

#[test]
fn a_program_used_in_an_earlier_life_outlives_an_unused_one_at_the_cap() {
    let (_tmp, path) = dir();
    let full: HashMap<String, i64> = (0..MAX_SEEN as i64)
        .map(|i| (format!("/usr/bin/p{i}"), T0 + i))
        .collect();
    write_merged(&path, &full, uid()).unwrap();
    // First life: p0, the oldest entry, is used again.
    let mut first = baselined(&path, &[]);
    first.observe(["/usr/bin/p0"], T0 + 1_000_000);
    drop(first);
    // Second life: a new program is acknowledged, so one entry must go.
    let mut second = baselined(&path, &[]);
    second.observe(["/usr/bin/fresh"], T0 + 2_000_000);
    assert!(second.acknowledge(T0 + 2_000_000));
    assert!(second.sync());
    let Loaded::Valid(saved) = read(&path, uid()) else {
        panic!("unreadable")
    };
    assert_eq!(saved.len(), MAX_SEEN);
    assert!(
        saved.contains_key("/usr/bin/p0"),
        "the recently used program was evicted"
    );
    assert!(
        !saved.contains_key("/usr/bin/p1"),
        "the least recently used stayed"
    );
    assert!(saved.contains_key("/usr/bin/fresh"));
}

fn hold_lock(path: &Path) -> std::fs::File {
    let (held, _) = crate::learned::acquire_lock(
        &crate::learned::lock_path_for(path),
        &crate::learned::LockPolicy::default(),
    );
    held.expect("the test could not take the lock")
}

fn saved(path: &Path) -> HashMap<String, i64> {
    match read(path, uid()) {
        Loaded::Valid(saved) => saved,
        other => panic!("{other:?}"),
    }
}

#[test]
fn without_the_lock_nothing_is_written_and_the_save_is_retried() {
    let (_tmp, path) = dir();
    let mut state = baselined(&path, &["/usr/bin/a"]);
    let held = hold_lock(&path);
    let ours = HashMap::from([("/usr/bin/blind".to_string(), T0)]);
    let refused = write_merged(&path, &ours, uid()).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert!(
        !saved(&path).contains_key("/usr/bin/blind"),
        "written without the lock"
    );

    state.observe(["/usr/bin/late"], T0 + 1);
    state.acknowledge(T0 + 2);
    assert!(
        !state.sync(),
        "the contended save was dropped or written blind"
    );
    assert!(!saved(&path).contains_key("/usr/bin/late"));
    drop(held);
    assert!(state.sync(), "the kept save was not retried");
    assert!(saved(&path).contains_key("/usr/bin/late"));
}

#[test]
fn observing_never_waits_for_a_held_lock() {
    let (_tmp, path) = dir();
    let mut state = baselined(&path, &["/usr/bin/a"]);
    let _held = hold_lock(&path);
    let started = std::time::Instant::now();
    state.observe(["/usr/bin/a"], T0 + TOUCH_PERSIST_MS);
    state.observe(["/usr/bin/late"], T0 + TOUCH_PERSIST_MS);
    state.acknowledge(T0 + TOUCH_PERSIST_MS + 1);
    let elapsed = started.elapsed();
    // The lock policy alone retries for ~0.5 s on the writer's side.
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "{elapsed:?}"
    );
}

#[test]
fn another_guis_acknowledgement_never_counts_as_this_ones() {
    use crate::connections::attention::AttentionFacts;
    let (_tmp, path) = dir();
    let mut a = baselined(&path, &["/usr/bin/a"]);
    a.observe(["/usr/bin/x"], T0);
    assert!(a.is_new("/usr/bin/x"));
    // GUI B acknowledges X and saves it.
    let mut b = baselined(&path, &["/usr/bin/a"]);
    b.observe(["/usr/bin/x"], T0);
    assert!(b.acknowledge(T0 + 1) && b.sync());
    assert!(saved(&path).contains_key("/usr/bin/x"));
    // A's periodic save merges with that file, then its rules refresh.
    a.observe(["/usr/bin/a"], T0 + TOUCH_PERSIST_MS);
    assert!(a.sync());
    assert!(a.is_new("/usr/bin/x"), "X stopped being new in A");
    let rule = crate::rules::row_store::Rule {
        name: "snitchwatch-default-x".to_string(),
        enabled: true,
        toggleable: Some(true),
        ..Default::default()
    };
    let facts = AttentionFacts::new(&[rule], a.seen_paths());
    let row = snitchwatch_bridge::ws_messages::ConnectionRow {
        id: "x1".to_string(),
        process: "x".to_string(),
        process_path: Some("/usr/bin/x".to_string()),
        dst_host: "h".to_string(),
        dst_ip: "10.0.0.1".to_string(),
        dst_port: 443,
        protocol: "tcp".to_string(),
        direction: "outgoing".to_string(),
        action: Some("allow".to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        matched_rule: Some("snitchwatch-default-x".to_string()),
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    };
    assert!(
        !facts.is_handled(&row),
        "X's rows collapse in A without A's acknowledgement"
    );
}

mod replaced {
    use super::*;
    use crate::logging::replace_tests::{put, replace};

    fn seen_file() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = tmp.path().join(SEEN_FILE_NAME);
        put(&path, br#"{"version":1,"programs":[]}"#, 0o600);
        (tmp, path)
    }

    const NEW: &[u8] = br#"{"version":1,"programs":[{"path":"/a","last_seen_ms":1}]}"#;

    #[test]
    fn zero_links_then_a_valid_replacement_is_read() {
        let (_d, path) = seen_file();
        let mut calls = 0;
        let loaded = read_with(&path, uid(), 4, &mut || {
            calls += 1;
            if calls == 1 {
                replace(&path, NEW, 0o600);
            }
        });
        assert_eq!(
            loaded,
            Loaded::Valid(HashMap::from([("/a".to_string(), 1)]))
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn zero_links_then_an_unsafe_or_hard_linked_replacement_is_foreign() {
        let (_d, path) = seen_file();
        let mut first = true;
        let loaded = read_with(&path, uid(), 4, &mut || {
            if std::mem::take(&mut first) {
                replace(&path, NEW, 0o666);
            }
        });
        assert_eq!(loaded, Loaded::Foreign);
        let (d, path) = seen_file();
        let mut first = true;
        let loaded = read_with(&path, uid(), 4, &mut || {
            if std::mem::take(&mut first) {
                replace(&path, NEW, 0o600);
                std::fs::hard_link(&path, d.path().join("other")).unwrap();
            }
        });
        assert_eq!(loaded, Loaded::Foreign);
    }

    #[test]
    fn zero_links_then_permanent_deletion_is_missing() {
        let (_d, path) = seen_file();
        let loaded = read_with(&path, uid(), 4, &mut || {
            let _ = std::fs::remove_file(&path);
        });
        assert_eq!(loaded, Loaded::Missing);
    }

    #[test]
    fn endless_replacement_is_unreadable_after_exactly_the_cap() {
        let (_d, path) = seen_file();
        let mut calls = 0;
        let loaded = read_with(&path, uid(), 5, &mut || {
            calls += 1;
            replace(&path, NEW, 0o600);
        });
        assert_eq!(loaded, Loaded::Unreadable);
        assert_eq!(calls, 5);
    }

    #[test]
    fn a_rename_after_the_accepting_fstat_is_not_foreign() {
        use crate::logging::replace_tests::OnCheck;
        let (_d, path) = seen_file();
        let loaded = read_with(&path, uid(), 4, &mut OnCheck(|| replace(&path, NEW, 0o600)));
        assert_eq!(loaded, Loaded::Valid(HashMap::new()));
    }
}
