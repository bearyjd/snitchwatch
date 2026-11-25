//! PR 10: clear and delete through the gate. Judged by what a restart and a
//! scan of the directory can see, not by what the code says it did.

use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use super::control::{Limits, Refused};
use super::control_support::{conn, count, event, start_after_drop};
use super::control_tests::{feed, gate_rig, settings_json, wait_for, GateRig};

const CANARY: &str = "canary-9d41.example";

pub(super) fn rows(rig: &GateRig) -> i64 {
    count(&rig.location, "SELECT COUNT(*) FROM events")
}

pub(super) fn feed_canary(rig: &GateRig, n: usize) {
    let capture = rig.gate.slot().get().expect("recording");
    let events: Vec<_> = (0..n)
        .map(|i| {
            event(
                "/opt/canary-9d41/bin",
                &format!("{i}-{CANARY}"),
                30 + i as i64,
            )
        })
        .collect();
    capture.observe(&events.iter().collect::<Vec<_>>());
}

pub(super) fn wait_rows(rig: &GateRig, n: i64) {
    wait_for("rows to be written", || rows(rig) == n);
}

/// Every file in the directory, any name, that holds the canary.
fn leaked(rig: &GateRig) -> Vec<String> {
    fs::read_dir(rig.location.dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| fs::read(p).is_ok_and(|b| b.windows(8).any(|w| w == b"canary-9")))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn clear_removes_the_previewed_history_and_recording_resumes() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 3);
    wait_rows(&rig, 3);
    assert_eq!(leaked(&rig), ["history.db"]);
    let before = rig.gate.generation().current();
    let preview = rig.gate.preview().unwrap();
    assert_eq!(preview.count, 3);
    rig.gate.clear(preview.token).unwrap();
    assert_eq!(rows(&rig), 0);
    assert_eq!(leaked(&rig), Vec::<String>::new(), "no canary in any file");
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM sessions"), 1);
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM gaps"), 0);
    assert!(settings_json(&rig).contains("\"enabled\":true"));
    assert!(settings_json(&rig).contains("\"text_id\":\"t\""));
    assert_eq!(rig.gate.generation().current(), before + 1);
    // Recording resumes on the new store, under the new generation.
    feed(&rig, 1);
    wait_rows(&rig, 1);
    assert!(rig.gate.readers().try_acquire().is_ok());
}

#[test]
fn clear_keeps_what_was_accepted_after_the_preview() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let preview = rig.gate.preview().unwrap();
    feed(&rig, 1);
    wait_rows(&rig, 3);
    rig.gate.clear(preview.token).unwrap();
    assert_eq!(
        rows(&rig),
        1,
        "the later event is carried, the previewed two are gone"
    );
    assert_eq!(leaked(&rig), Vec::<String>::new());
    assert_eq!(
        count(
            &rig.location,
            "SELECT COUNT(*) FROM events WHERE dst_host = 'h0.example'"
        ),
        1
    );
}

#[test]
fn a_token_is_refused_when_forged_expired_or_from_an_older_generation() {
    let rig = gate_rig(Limits {
        token_ttl: Duration::from_millis(50),
        ..Limits::default()
    });
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let capture = rig.gate.slot().get().unwrap();
    let preview = rig.gate.preview().unwrap();
    let forged = super::control_preview::TokenId(preview.token.0 ^ 1);
    assert_eq!(rig.gate.clear(forged), Err(Refused::StaleToken));
    // Expiry is the one thing only time can show.
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(rig.gate.clear(preview.token), Err(Refused::StaleToken));
    assert_eq!(rows(&rig), 2);
    assert!(std::sync::Arc::ptr_eq(
        &capture,
        &rig.gate.slot().get().unwrap()
    ));
    assert!(capture.begin_decision(&conn("/a", "h")).is_some());
}

#[test]
fn a_token_dies_with_the_generation_it_was_issued_under() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let first = rig.gate.preview().unwrap();
    let second = rig.gate.preview().unwrap();
    rig.gate.clear(first.token).unwrap();
    assert_eq!(rig.gate.clear(second.token), Err(Refused::StaleToken));
}

#[test]
fn a_changed_previewed_set_is_refused_and_recording_continues() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let preview = rig.gate.preview().unwrap();
    let db = rusqlite::Connection::open(rig.location.db_path()).unwrap();
    db.execute(
        "UPDATE events SET repeat_count = repeat_count + 1 WHERE id = 1",
        [],
    )
    .unwrap();
    drop(db);
    let before = rig.gate.generation().current();
    assert_eq!(rig.gate.clear(preview.token), Err(Refused::Changed));
    assert_eq!(rows(&rig), 2, "nothing deleted");
    assert!(rig
        .gate
        .slot()
        .get()
        .expect("recording restarted")
        .begin_decision(&conn("/a", "h"))
        .is_some());
    assert!(
        rig.gate.generation().current() > before,
        "the old cursors stay dead"
    );
    assert!(rig.gate.readers().try_acquire().is_ok());
}

#[test]
fn a_journal_left_beside_the_store_is_refused() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let preview = rig.gate.preview().unwrap();
    fs::write(rig.location.dir().join("history.db-shm"), b"").unwrap();
    assert!(matches!(
        rig.gate.clear(preview.token),
        Err(Refused::Files(_))
    ));
    assert_eq!(rows(&rig), 1);
    assert!(rig.gate.slot().get().is_some());
}

#[test]
fn delete_removes_the_store_and_its_settings_and_nothing_else() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let dir = rig.location.dir().to_owned();
    fs::write(dir.join("other.txt"), "decoy").unwrap();
    let outside = rig.root.path().join("outside.txt");
    fs::write(&outside, "keep").unwrap();
    symlink(&outside, dir.join("decoy.lnk")).unwrap();
    let preview = rig.gate.preview().unwrap();
    rig.gate.delete(preview.token).unwrap();
    assert!(rig.gate.slot().get().is_none());
    let mut left: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["decoy.lnk", "history.lock", "other.txt"]);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");
    // Off for good: a restart records nothing and creates nothing.
    drop(rig.gate);
    let again = start_after_drop(&rig.state, Limits::default());
    assert!(again.slot().get().is_none());
    assert!(!dir.join("history.db").exists());
}

#[test]
fn delete_refuses_a_linked_store_and_leaves_everything_as_it_was() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let preview = rig.gate.preview().unwrap();
    let dir = rig.location.dir().to_owned();
    // The live name becomes a link, while the writer keeps its open file.
    let real = dir.join("real.db");
    let victim = rig.root.path().join("victim.db");
    fs::write(&victim, "keep").unwrap();
    fs::rename(dir.join("history.db"), &real).unwrap();
    symlink(&victim, dir.join("history.db")).unwrap();
    // The link is refused before anything is closed: recording goes on.
    let result = rig.gate.delete(preview.token);
    assert!(
        matches!(&result, Err(Refused::Files(reason)) if reason.contains("not a plain file")),
        "{result:?}"
    );
    assert!(rig.gate.slot().get().is_some(), "recording untouched");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    assert!(real.exists());
    assert!(dir.join("settings.json").exists(), "settings untouched");
    assert!(settings_json(&rig).contains("\"enabled\":true"));
}

#[test]
fn a_linked_history_directory_makes_history_unavailable_and_touches_nothing() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    drop(rig.gate);
    let real = rig.state.join("real-history");
    fs::rename(rig.location.dir(), &real).unwrap();
    symlink(&real, rig.location.dir()).unwrap();
    let gate = start_after_drop(&rig.state, Limits::default());
    assert!(gate.slot().get().is_none());
    assert!(matches!(gate.preview(), Err(Refused::Unavailable(_))));
    assert!(real.join("history.db").exists());
}
