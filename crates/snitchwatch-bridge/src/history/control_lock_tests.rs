//! PR 10 (#418): the single-writer lock.

use std::fs;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::control_files::{FilesError, StoreDir};
use super::control_lock::{LockError, LOCK_FILE};
use super::open::effective_uid;
use super::test_support::{state_dir, tempdir};

const CHILD_DIR: &str = "SW_CONTROL_LOCK_DIR";
const READY: &str = "SW_CONTROL_LOCK_READY";

fn history_dir() -> (tempfile::TempDir, PathBuf) {
    let root = tempdir();
    let dir = state_dir(&root).join("history");
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    (root, dir)
}

fn open(dir: &Path) -> StoreDir {
    StoreDir::open(dir, effective_uid()).unwrap()
}

#[test]
fn a_second_holder_is_refused_until_the_first_lets_go() {
    let (_root, dir) = history_dir();
    let first = open(&dir).lock_writer().unwrap();
    let second = open(&dir).lock_writer();
    assert!(matches!(second, Err(LockError::InUse)), "{second:?}");
    drop(first);
    open(&dir).lock_writer().unwrap();
}

#[test]
fn the_lock_file_is_private_and_the_directory_is_never_created() {
    let (root, dir) = history_dir();
    let _held = open(&dir).lock_writer().unwrap();
    let mode = fs::metadata(dir.join(LOCK_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let missing = root.path().join("nowhere");
    assert!(StoreDir::open(&missing, effective_uid()).is_err());
    assert!(!missing.exists());
}

#[test]
fn a_lock_file_that_is_a_link_is_refused_and_its_target_left_alone() {
    let (root, dir) = history_dir();
    let target = root.path().join("victim.txt");
    fs::write(&target, "keep").unwrap();
    symlink(&target, dir.join(LOCK_FILE)).unwrap();
    let got = open(&dir).lock_writer();
    assert!(
        matches!(got, Err(LockError::Files(FilesError::NotPlain(_)))),
        "{got:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
}

/// The child: takes the lock, says so, and waits to be killed.
#[test]
fn lock_holder_child() {
    let (Ok(dir), Ok(ready)) = (std::env::var(CHILD_DIR), std::env::var(READY)) else {
        return;
    };
    let _held = open(Path::new(&dir)).lock_writer().unwrap();
    fs::write(ready, "x").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

fn spawn_holder(dir: &Path, ready: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "history::control_lock_tests::lock_holder_child"])
        .args(["--test-threads=1", "--nocapture"])
        .env(CHILD_DIR, dir)
        .env(READY, ready)
        .spawn()
        .unwrap()
}

#[test]
fn a_killed_holder_releases_the_lock_with_its_descriptor() {
    let (root, dir) = history_dir();
    let ready = root.path().join("ready");
    let mut child = spawn_holder(&dir, &ready);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "the holder never took the lock");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(open(&dir).lock_writer(), Err(LockError::InUse)));
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    open(&dir).lock_writer().unwrap();
}

#[test]
fn a_second_bridge_on_the_same_state_records_nothing_and_leaves_the_first_alone() {
    use super::capture::start;
    use super::control_support::{consent, count};
    use super::{enable, locate, Mode};
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let first = start(Mode::User, Some(&state)).expect("the first bridge records");
    let open_sessions = "SELECT COUNT(*) FROM sessions WHERE end_ms IS NULL";
    assert_eq!(count(&location, open_sessions), 1);
    let watermark = count(&location, "SELECT watermark_ms FROM meta");
    assert!(
        start(Mode::User, Some(&state)).is_none(),
        "the second must refuse"
    );
    assert_eq!(
        count(&location, open_sessions),
        1,
        "its session is still open"
    );
    assert_eq!(count(&location, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(count(&location, "SELECT watermark_ms FROM meta"), watermark);
    drop(first);
    // The lock went with the capture: a later start records again.
    assert!(start(Mode::User, Some(&state)).is_some());
}
