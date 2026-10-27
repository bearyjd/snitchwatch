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
