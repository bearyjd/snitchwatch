//! PR 10: a `kill -9` at every step of a clear. A child process runs the
//! real gate and kills itself after the named file step; this process
//! restarts a gate on what is left, enabled and disabled.

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use super::control::{ControlGate, Limits};
use super::control_files::Step;
use super::control_support::{consent, count, event};
use super::test_support::{state_dir, tempdir};
use super::{enable, locate, settings, Location, Mode};

const KILL_AT: &str = "SW_CONTROL_CRASH_AT";
const STATE: &str = "SW_CONTROL_CRASH_STATE";

fn dir_canaries(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| fs::read(p).is_ok_and(|b| b.windows(8).any(|w| w == b"canary-9")))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    all.sort();
    all
}

/// The child: records three canary events, previews, clears, and is killed
/// after the named step.
#[test]
fn clear_child() {
    let (Ok(at), Ok(state)) = (std::env::var(KILL_AT), std::env::var(STATE)) else {
        return;
    };
    let gate = ControlGate::start(Mode::User, Some(Path::new(&state)), Limits::default());
    let capture = gate.slot().get().expect("recording");
    let events: Vec<_> = (0..3)
        .map(|i| {
            event(
                "/opt/canary-9d41/bin",
                &format!("{i}.canary-9d41.example"),
                30 + i,
            )
        })
        .collect();
    capture.observe(&events.iter().collect::<Vec<_>>());
    let location = locate(Mode::User, Some(Path::new(&state))).unwrap();
    while count(&location, "SELECT COUNT(*) FROM events") < 3 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let token = gate.preview().unwrap().token;
    let kill = |step: Step| {
        if format!("{step:?}") == at {
            // SAFETY: kill(2) on our own pid; no memory is touched.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
    };
    let _ = gate.clear_observed(token, &kill);
}

fn crashed_clear(step: &str) -> (tempfile::TempDir, std::path::PathBuf, Location) {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "history::control_crash_tests::clear_child"])
        .args(["--test-threads=1", "--nocapture"])
        .env(KILL_AT, step)
        .env(STATE, &state)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{step}: {status:?}");
    (root, state, location)
}

#[test]
fn a_kill_at_every_step_leaves_the_old_store_whole_or_an_empty_one() {
    for step in ["Begin", "Renamed", "TombstoneRemoved"] {
        let (_root, state, location) = crashed_clear(step);
        let dir = location.dir().to_owned();
        let live = dir.join("history.db");
        if step == "Begin" {
            assert!(live.exists(), "{step}: the old store is still whole");
        } else {
            assert!(!live.exists(), "{step}: nothing under the live name");
        }
        // Restart, history on: the old store whole, or a fresh empty one.
        let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
        assert!(gate.slot().get().is_some(), "{step}: recording again");
        let events = count(&location, "SELECT COUNT(*) FROM events");
        if step == "Begin" {
            assert_eq!(events, 3, "{step}");
        } else {
            assert_eq!(events, 0, "{step}");
            assert_eq!(dir_canaries(&dir), Vec::<String>::new(), "{step}");
        }
        assert!(!dir.join("history.db.deleting").exists(), "{step}");
    }
}

#[test]
fn a_history_off_restart_finishes_the_removal_and_creates_nothing() {
    for step in ["Renamed", "TombstoneRemoved"] {
        let (_root, state, location) = crashed_clear(step);
        let dir = location.dir().to_owned();
        let mut current = settings::load(&location).unwrap().unwrap();
        current.enabled = false;
        settings::save(&location, &current).unwrap();
        let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
        assert!(gate.slot().get().is_none());
        assert_eq!(dir_canaries(&dir), Vec::<String>::new(), "{step}");
        assert_eq!(names(&dir), ["history.lock", "settings.json"], "{step}");
    }
}
