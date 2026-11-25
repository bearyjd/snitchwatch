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
use super::reader::{read_page, Context, Limits as ReadLimits, Outcome, Request};
use super::reader_registry::CancelToken;
use super::test_support::{state_dir, tempdir};
use super::timeline::Kind;
use super::{enable, locate, settings, Location, Mode};

const KILL_AT: &str = "SW_CONTROL_CRASH_AT";
const STATE: &str = "SW_CONTROL_CRASH_STATE";
const KIND: &str = "SW_CONTROL_CRASH_KIND";

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
    // Accepted after the preview: a clear carries it into the new store.
    let later = event("/usr/bin/curl", "carry.example", 99);
    capture.observe(&[&later]);
    while count(&location, "SELECT COUNT(*) FROM events") < 4 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let kill = |step: Step| {
        if format!("{step:?}") == at {
            // SAFETY: kill(2) on our own pid; no memory is touched.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
    };
    if std::env::var(KIND).is_ok_and(|k| k == "delete") {
        let _ = gate.delete_observed(token, &kill);
    } else {
        let _ = gate.clear_observed(token, &kill);
    }
}

/// The child of the disable test: kills itself as the writer's stop begins,
/// the writer being blocked behind another connection's lock.
#[test]
fn disable_child() {
    let (Ok(_), Ok(state)) = (
        std::env::var("SW_CONTROL_CRASH_DISABLE"),
        std::env::var(STATE),
    ) else {
        return;
    };
    let gate = ControlGate::start(Mode::User, Some(Path::new(&state)), Limits::default());
    let location = locate(Mode::User, Some(Path::new(&state))).unwrap();
    let blocker = rusqlite::Connection::open(location.db_path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let slot = gate.slot();
    std::thread::spawn(move || {
        // The capture leaves the slot as the writer's stop begins.
        while slot.get().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // SAFETY: kill(2) on our own pid; no memory is touched.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    });
    let _ = gate.disable();
}

fn crashed_clear(step: &str) -> (tempfile::TempDir, std::path::PathBuf, Location) {
    crashed(step, "clear")
}

fn crashed(step: &str, kind: &str) -> (tempfile::TempDir, std::path::PathBuf, Location) {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "history::control_crash_tests::clear_child"])
        .args(["--test-threads=1", "--nocapture"])
        .env(KILL_AT, step)
        .env(STATE, &state)
        .env(KIND, kind)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{step}: {status:?}");
    (root, state, location)
}

const TOMBSTONE: &str = "history.db.deleting";

/// What the directory holds at the instant of the kill, before any restart.
fn assert_at_the_kill(step: &str, dir: &Path, location: &Location) {
    let live = dir.join("history.db");
    match step {
        "Begin" => {
            assert_eq!(
                dir_canaries(dir),
                ["history.db"],
                "{step}: positive control"
            );
            assert_eq!(count(location, "SELECT COUNT(*) FROM events"), 4, "{step}");
        }
        "Renamed" => {
            assert!(!live.exists(), "{step}: nothing under the live name");
            assert_eq!(dir_canaries(dir), [TOMBSTONE], "{step}: only the tombstone");
        }
        "TombstoneRemoved" => {
            assert!(!live.exists(), "{step}: nothing under the live name");
            assert_eq!(
                dir_canaries(dir),
                Vec::<String>::new(),
                "{step}: it is gone"
            );
        }
        "Created" => {
            assert_eq!(dir_canaries(dir), Vec::<String>::new(), "{step}");
            assert_eq!(count(location, "SELECT COUNT(*) FROM events"), 0, "{step}");
        }
        _ => {
            assert_eq!(dir_canaries(dir), Vec::<String>::new(), "{step}");
            let kept = "SELECT COUNT(*) FROM events WHERE dst_host = 'carry.example'";
            assert_eq!(count(location, "SELECT COUNT(*) FROM events"), 1, "{step}");
            assert_eq!(count(location, kept), 1, "{step}: the carried row");
        }
    }
}

#[test]
fn a_kill_at_every_step_leaves_the_old_store_whole_or_an_empty_one() {
    for step in ["Begin", "Renamed", "TombstoneRemoved", "Created", "Carried"] {
        let (_root, state, location) = crashed_clear(step);
        let dir = location.dir().to_owned();
        assert_at_the_kill(step, &dir, &location);
        // Restart, history on: the old store whole, or a fresh empty one.
        let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
        assert!(gate.slot().get().is_some(), "{step}: recording again");
        let sessions = if step == "Begin" { 2 } else { 1 };
        assert_eq!(
            count(&location, "SELECT COUNT(*) FROM sessions"),
            sessions,
            "{step}"
        );
        let events = count(&location, "SELECT COUNT(*) FROM events");
        let expected = match step {
            "Begin" => 4,
            "Carried" => 1,
            _ => 0,
        };
        assert_eq!(events, expected, "{step}");
        if step != "Begin" {
            assert_eq!(dir_canaries(&dir), Vec::<String>::new(), "{step}");
        }
        assert!(!dir.join(TOMBSTONE).exists(), "{step}");
        assert_crash_not_recorded(&gate, &location, step);
    }
}

/// The reader, after the restart, never calls the stretch between the kill
/// and the new session recorded coverage.
fn assert_crash_not_recorded(gate: &ControlGate, location: &Location, step: &str) {
    let restart = count(location, "SELECT MAX(start_ms) FROM sessions");
    let generation = gate.generation();
    let cancel = CancelToken::default();
    let ctx = Context {
        location,
        generation: &generation,
        cancel: &cancel,
        now_ms: restart + 1,
        live_session_start_ms: None,
        standing: None,
        limits: ReadLimits::default(),
    };
    let Outcome::Page(page) = read_page(
        &Request {
            cursor: None,
            limit: 10,
        },
        &ctx,
    ) else {
        panic!("{step}: no page");
    };
    // Whatever the page shows of the instants before the new session is not
    // recorded coverage: the old session crashed, or its store is gone.
    let near: Vec<_> = page
        .coverage
        .iter()
        .filter(|s| s.start_ms < restart)
        .collect();
    for segment in &near {
        assert!(
            !matches!(segment.kind, Kind::Recorded) || segment.end_ms < restart - 1,
            "{step}: {segment:?} claims recording across the crash"
        );
    }
    let gap_before = near.iter().any(|s| matches!(s.kind, Kind::Gap));
    match step {
        // The old session is in the old store: its end is a crash gap.
        "Begin" => assert!(gap_before, "{step}: the crash is a gap: {near:?}"),
        // The carried event is older than the new store: shown, unexplained.
        "Carried" => assert!(!near.is_empty(), "{step}: the carried row has a place"),
        // The old store is gone: nothing of it is claimed.
        _ => assert!(
            near.is_empty(),
            "{step}: nothing before the restart: {near:?}"
        ),
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

#[test]
fn a_kill_between_saving_off_and_stopping_the_writer_does_not_restart_recording() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "history::control_crash_tests::disable_child"])
        .args(["--test-threads=1", "--nocapture"])
        .env("SW_CONTROL_CRASH_DISABLE", "1")
        .env(STATE, &state)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
    assert!(
        gate.slot().get().is_none(),
        "the saved off survives the kill"
    );
    assert_eq!(
        count(&location, "SELECT COUNT(*) FROM events"),
        0,
        "the store still opens"
    );
}

#[test]
fn a_kill_at_every_step_of_a_delete_leaves_the_store_whole_or_gone_and_history_off() {
    for step in ["Begin", "Renamed", "TombstoneRemoved"] {
        let (_root, state, location) = crashed(step, "delete");
        let dir = location.dir().to_owned();
        let saved = settings::load(&location).unwrap().unwrap();
        assert!(
            !saved.enabled,
            "{step}: off was saved before anything moved"
        );
        assert!(saved.consent.is_none(), "{step}: delete revokes consent");
        let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
        assert!(gate.slot().get().is_none(), "{step}: never records again");
        assert!(!dir.join("history.db.deleting").exists(), "{step}");
        if step == "Begin" {
            assert!(dir.join("history.db").exists(), "{step}: whole");
        } else {
            assert!(!dir.join("history.db").exists(), "{step}: none");
            assert_eq!(dir_canaries(&dir), Vec::<String>::new(), "{step}");
        }
    }
}
