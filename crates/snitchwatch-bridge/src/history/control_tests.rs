//! PR 10: the control gate, disable first. Every operation is judged by what
//! is observable afterwards: rows, ledger, settings, generation, readers.

use std::fs;
use std::time::{Duration, Instant};

use super::control::{ControlGate, Limits, Refused};
use super::control_support::{consent, count, event};
use super::reader_registry::NotAdmitted;
use super::test_support::{state_dir, tempdir};
use super::{enable, locate, Location, Mode};

pub(super) struct GateRig {
    pub root: tempfile::TempDir,
    pub state: std::path::PathBuf,
    pub location: Location,
    pub gate: ControlGate,
}

pub(super) fn gate_rig(limits: Limits) -> GateRig {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let gate = ControlGate::start(Mode::User, Some(&state), limits);
    GateRig {
        root,
        state,
        location,
        gate,
    }
}

pub(super) fn feed(rig: &GateRig, n: usize) {
    let capture = rig.gate.slot().get().expect("recording");
    let events: Vec<_> = (0..n)
        .map(|i| event("/usr/bin/curl", &format!("h{i}.example"), 20 + i as i64))
        .collect();
    capture.observe(&events.iter().collect::<Vec<_>>());
}

pub(super) fn settings_json(rig: &GateRig) -> String {
    fs::read_to_string(rig.location.settings_path()).unwrap()
}

pub(super) fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn disable_keeps_history_and_settings_and_advances_the_generation() {
    let rig = gate_rig(Limits::default());
    let before = rig.gate.generation().current();
    feed(&rig, 3);
    rig.gate.disable().unwrap();
    assert!(rig.gate.slot().get().is_none(), "recording stopped");
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM events"), 3);
    assert_eq!(
        count(
            &rig.location,
            "SELECT COUNT(*) FROM sessions WHERE end_kind = 'disabled'"
        ),
        1
    );
    let settings = settings_json(&rig);
    assert!(settings.contains("\"enabled\":false"), "{settings}");
    assert!(
        settings.contains("\"text_id\":\"t\""),
        "consent kept: {settings}"
    );
    assert_eq!(rig.gate.generation().current(), before + 1);
    assert!(rig.gate.readers().try_acquire().is_ok(), "readers reopened");
}

#[test]
fn a_disable_blocked_in_the_writer_join_holds_nothing_hostage() {
    let rig = gate_rig(Limits::default());
    let capture = rig.gate.slot().get().unwrap();
    feed(&rig, 2);
    // The slow disk: another connection holds the write lock.
    let blocker = rusqlite::Connection::open(rig.location.db_path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let before = rig.gate.generation().current();
    let gate = rig.gate.clone();
    let op = std::thread::spawn(move || gate.disable());
    wait_for("the generation to advance", || {
        rig.gate.generation().current() > before
    });
    // The new mode is durable before the writer is stopped (crash safety):
    // the join is still blocked behind the lock while settings say off.
    wait_for("the settings to say off", || {
        settings_json(&rig).contains("\"enabled\":false")
    });
    assert!(!op.is_finished(), "the writer join is still waiting");
    // Closed admission, and a second operation is refused at once.
    assert_eq!(
        rig.gate.readers().try_acquire().err(),
        Some(NotAdmitted::ControlOperation)
    );
    assert_eq!(rig.gate.disable(), Err(Refused::InProgress));
    // `Shared::control` is not held across the join: a prompt is refused at
    // once instead of waiting for the writer.
    // Wait until the writer's stop is under way (the queue refuses events),
    // so the join is what the prompt would have to wait behind.
    let probe = event("/usr/bin/curl", "probe.example", 99);
    wait_for("the queue to close", || {
        let at = Instant::now();
        capture.observe(&[&probe]);
        assert!(at.elapsed() < Duration::from_millis(500), "observe waited");
        capture.stats().dropped_queue > 0
    });
    let started = Instant::now();
    assert!(capture
        .begin_decision(&super::control_support::conn("/a", "h"))
        .is_none());
    assert!(started.elapsed() < Duration::from_millis(500));
    blocker.execute_batch("ROLLBACK;").unwrap();
    op.join().unwrap().unwrap();
}

#[test]
fn a_reader_that_will_not_stop_fails_the_disable_closed() {
    let limits = Limits {
        quiesce: Duration::from_millis(100),
        extension: Duration::from_millis(100),
        ..Limits::default()
    };
    let rig = gate_rig(limits);
    let stuck = rig.gate.readers().try_acquire().unwrap();
    let before = rig.gate.generation().current();
    let settings = settings_json(&rig);
    assert_eq!(rig.gate.disable(), Err(Refused::ReaderBusy));
    assert_eq!(rig.gate.generation().current(), before, "nothing advanced");
    assert_eq!(settings_json(&rig), settings, "nothing saved");
    let capture = rig.gate.slot().get().expect("still recording");
    assert!(capture
        .begin_decision(&super::control_support::conn("/a", "h"))
        .is_some());
    drop(stuck);
    assert!(rig.gate.readers().try_acquire().is_ok(), "readers reopened");
}

#[test]
fn a_settings_failure_restores_recording() {
    use std::os::unix::fs::PermissionsExt;
    let rig = gate_rig(Limits::default());
    let dir = rig.location.dir().to_owned();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let result = rig.gate.disable();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(Refused::Settings(_))), "{result:?}");
    let capture = rig.gate.slot().get().expect("recording continues");
    assert!(capture
        .begin_decision(&super::control_support::conn("/a", "h"))
        .is_some());
    assert!(settings_json(&rig).contains("\"enabled\":true"));
    let _ = &rig.state;
}
