//! PR 10: the file operations remove exactly the store's own names, through
//! no link, and a kill at any step leaves the old store whole or none.

use std::fs;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::control_files::{FilesError, Step, StoreDir, TOMBSTONE};
use super::open::effective_uid;
use super::test_support::tempdir;

const CANARY: &str = "CANARY-7f3a-secret-host.example";
const KILL_AT: &str = "SW_CONTROL_FILES_KILL_AT";
const DIR: &str = "SW_CONTROL_FILES_DIR";

/// A 0700 history directory holding a store with a canary, and decoys.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempdir();
    let dir = root.path().canonicalize().unwrap().join("history");
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dir.join("history.db"), format!("db {CANARY}")).unwrap();
    fs::write(dir.join("settings.json"), "{\"enabled\":true}").unwrap();
    fs::write(dir.join("history.lock"), "").unwrap();
    fs::write(dir.join("other.txt"), "decoy").unwrap();
    (root, dir)
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    all.sort();
    all
}

fn canary_files(dir: &Path) -> Vec<String> {
    names(dir)
        .into_iter()
        .filter(|n| fs::read_to_string(dir.join(n)).is_ok_and(|t| t.contains(CANARY)))
        .collect()
}

fn open(dir: &Path) -> StoreDir {
    StoreDir::open(dir, effective_uid()).unwrap()
}

#[test]
fn removal_takes_exactly_the_store_and_nothing_beside_it() {
    let (root, dir) = fixture();
    let outside = root.path().join("outside.txt");
    fs::write(&outside, CANARY).unwrap();
    symlink(&outside, dir.join("decoy.lnk")).unwrap();
    open(&dir).remove_store(&|_| {}).unwrap();
    assert_eq!(
        names(&dir),
        ["decoy.lnk", "history.lock", "other.txt", "settings.json"]
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), CANARY);
    assert_eq!(fs::read_to_string(dir.join("other.txt")).unwrap(), "decoy");
    assert!(canary_files(&dir).is_empty() || canary_files(&dir) == ["decoy.lnk"]);
}

#[test]
fn a_symlinked_store_is_refused_unfollowed_and_nothing_moves() {
    let (root, dir) = fixture();
    let target = root.path().join("elsewhere.db");
    fs::write(&target, CANARY).unwrap();
    fs::remove_file(dir.join("history.db")).unwrap();
    symlink(&target, dir.join("history.db")).unwrap();
    let before = names(&dir);
    let err = open(&dir).remove_store(&|_| {}).unwrap_err();
    assert!(matches!(err, FilesError::NotPlain("history.db")), "{err}");
    assert_eq!(names(&dir), before);
    assert_eq!(fs::read_to_string(&target).unwrap(), CANARY);
    assert!(fs::symlink_metadata(dir.join("history.db"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn a_symlinked_history_directory_is_refused() {
    let (root, dir) = fixture();
    let link = root.path().join("link");
    symlink(&dir, &link).unwrap();
    assert!(StoreDir::open(&link, effective_uid()).is_err());
    assert_eq!(canary_files(&dir), ["history.db"], "nothing was touched");
}

#[test]
fn a_leftover_sidecar_refuses_the_removal_before_anything_moves() {
    for sidecar in ["history.db-journal", "history.db-wal", "history.db-shm"] {
        let (_root, dir) = fixture();
        fs::write(dir.join(sidecar), "x").unwrap();
        let before = names(&dir);
        let err = open(&dir).remove_store(&|_| {}).unwrap_err();
        assert!(matches!(err, FilesError::Sidecar(_)), "{sidecar}: {err}");
        assert_eq!(names(&dir), before, "{sidecar}");
    }
}

#[test]
fn removing_settings_unlinks_settings_json_only() {
    let (_root, dir) = fixture();
    open(&dir).remove_settings().unwrap();
    assert_eq!(names(&dir), ["history.db", "history.lock", "other.txt"]);
    // Absent is fine.
    open(&dir).remove_settings().unwrap();
}

#[test]
fn leftovers_of_a_crashed_removal_are_finished_without_creating_anything() {
    let (_root, dir) = fixture();
    fs::rename(dir.join("history.db"), dir.join(TOMBSTONE)).unwrap();
    fs::write(dir.join("history.db-journal"), CANARY).unwrap();
    open(&dir).finish_leftover().unwrap();
    assert_eq!(names(&dir), ["history.lock", "other.txt", "settings.json"]);
    assert!(canary_files(&dir).is_empty());
}

#[test]
fn a_live_store_keeps_its_sidecars_when_leftovers_are_finished() {
    let (_root, dir) = fixture();
    fs::write(dir.join("history.db-journal"), "hot").unwrap();
    fs::write(dir.join(TOMBSTONE), CANARY).unwrap();
    open(&dir).finish_leftover().unwrap();
    assert_eq!(
        names(&dir),
        [
            "history.db",
            "history.db-journal",
            "history.lock",
            "other.txt",
            "settings.json"
        ]
    );
}

/// The child: removes the store and kills itself after the named step.
#[test]
fn removal_child() {
    let (Ok(at), Ok(dir)) = (std::env::var(KILL_AT), std::env::var(DIR)) else {
        return;
    };
    let kill = |step: Step| {
        if format!("{step:?}") == at {
            // SAFETY: kill(2) on our own pid; no memory is touched.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
    };
    let _ = open(Path::new(&dir)).remove_store(&kill);
}

fn kill_at(step: &str, dir: &Path) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "history::control_files_tests::removal_child"])
        .args(["--test-threads=1", "--nocapture"])
        .env(KILL_AT, step)
        .env(DIR, dir)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{step}: {status:?}");
}

#[test]
fn a_kill_at_every_step_leaves_the_old_store_whole_or_none() {
    for step in ["Begin", "Renamed", "TombstoneRemoved"] {
        let (_root, dir) = fixture();
        kill_at(step, &dir);
        let live = dir.join("history.db");
        if step == "Begin" {
            assert_eq!(fs::read_to_string(&live).unwrap(), format!("db {CANARY}"));
        } else {
            assert!(!live.exists(), "{step}: no store under the live name");
        }
        let tombstone = dir.join(TOMBSTONE);
        match step {
            "Renamed" => assert_eq!(
                fs::read_to_string(&tombstone).unwrap(),
                format!("db {CANARY}"),
                "{step}: the store is the tombstone, not yet unlinked"
            ),
            _ => assert!(!tombstone.exists(), "{step}: no tombstone"),
        }
        // The next start, enabled or not, finishes the job.
        open(&dir).finish_leftover().unwrap();
        let expected = if step == "Begin" {
            vec!["history.db".to_owned()]
        } else {
            vec![]
        };
        assert_eq!(canary_files(&dir), expected, "{step}");
        assert!(!dir.join(TOMBSTONE).exists(), "{step}");
        for kept in ["settings.json", "history.lock", "other.txt"] {
            assert!(dir.join(kept).exists(), "{step}: {kept}");
        }
    }
}

#[test]
fn a_history_off_start_finishes_a_crashed_removal_and_creates_nothing() {
    use super::capture::start;
    use super::{enable, locate, Mode};
    let root = tempdir();
    let state = super::test_support::state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, super::control_support::consent()).unwrap();
    let dir = location.dir().to_owned();
    // Disabled on disk, with what a crash mid-delete leaves behind.
    fs::remove_file(dir.join("settings.json")).unwrap();
    fs::write(dir.join(TOMBSTONE), CANARY).unwrap();
    fs::write(dir.join("history.db-journal"), CANARY).unwrap();
    assert!(start(Mode::User, Some(&state)).is_none());
    // Only the lock file the start took beside it; nothing else is created.
    assert_eq!(names(&dir), ["history.lock"]);
    // And with no history directory at all, none is created.
    let bare = tempdir();
    let bare_state = super::test_support::state_dir(&bare);
    assert!(start(Mode::User, Some(&bare_state)).is_none());
    assert!(!bare_state.join("history").exists());
}
