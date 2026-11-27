//! #444: the staged store's names. A leftover is removed by exactly those
//! names, a name that is not a plain file is refused and never followed, and
//! delete leaves none behind.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, FileTypeExt};
use std::path::Path;

use super::control::{Limits, Refused};
use super::control_clear_tests::{feed_canary, rows, wait_rows};
use super::control_files::{FilesError, Step, StoreDir};
use super::control_leftover_tests::failed_delete_at;
use super::control_support::start_after_drop;
use super::control_tests::{gate_rig, settings_json, GateRig};

const STAGED: [&str; 4] = [
    "history.db.new",
    "history.db.new-journal",
    "history.db.new-wal",
    "history.db.new-shm",
];
const CANARY: &str = "canary-9d41 left in a staged file";

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    all.sort();
    all
}

fn plant_all(dir: &Path) {
    for name in STAGED {
        fs::write(dir.join(name), CANARY).unwrap();
    }
}

fn open(dir: &Path) -> StoreDir {
    StoreDir::open(dir, super::open::effective_uid()).unwrap()
}

fn disabled_store() -> GateRig {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    rig.gate.disable().unwrap();
    rig
}

#[test]
fn a_start_removes_a_leftover_staged_store_and_leaves_the_store_alone() {
    let GateRig {
        root: _root,
        state,
        location,
        gate,
    } = disabled_store();
    drop(gate);
    let before = fs::read(location.db_path()).unwrap();
    plant_all(location.dir());
    let _gate = start_after_drop(&state, Limits::default());
    assert_eq!(
        names(location.dir()),
        ["history.db", "history.lock", "settings.json"]
    );
    assert_eq!(fs::read(location.db_path()).unwrap(), before);
}

#[test]
fn a_leftover_staged_name_with_a_second_link_is_reported() {
    for name in STAGED {
        let rig = disabled_store();
        let dir = rig.location.dir();
        fs::write(dir.join(name), CANARY).unwrap();
        fs::hard_link(dir.join(name), dir.join("other-name")).unwrap();
        let result = open(dir).finish_leftover();
        assert!(
            matches!(result, Err(FilesError::StagedLinked(n)) if n == name),
            "{name}: {result:?}"
        );
        assert!(!dir.join(name).exists(), "{name}");
    }
}

#[test]
fn a_directory_with_only_a_staged_store_has_history_to_own() {
    let rig = disabled_store();
    let dir = rig.location.dir();
    fs::remove_file(rig.location.db_path()).unwrap();
    fs::remove_file(rig.location.settings_path()).unwrap();
    assert!(!open(dir).has_history().unwrap());
    fs::write(dir.join(STAGED[0]), CANARY).unwrap();
    assert!(open(dir).has_history().unwrap());
}

#[test]
fn a_delete_leaves_nothing_of_a_leftover_staged_store() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    plant_all(rig.location.dir());
    rig.gate.delete(token).unwrap();
    assert_eq!(names(rig.location.dir()), ["history.lock"]);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Planted {
    Link,
    Fifo,
}

fn plant(kind: Planted, at: &Path, target: &Path) {
    match kind {
        Planted::Link => symlink(target, at).unwrap(),
        Planted::Fifo => {
            let c = CString::new(at.as_os_str().as_bytes()).unwrap();
            // SAFETY: a NUL-terminated path and a plain mode.
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        }
    }
}

fn still_there(kind: Planted, at: &Path) -> bool {
    let kind_there = fs::symlink_metadata(at).unwrap().file_type();
    match kind {
        Planted::Link => kind_there.is_symlink(),
        Planted::Fifo => kind_there.is_fifo(),
    }
}

#[test]
fn a_staged_name_that_is_not_a_plain_file_is_refused_unfollowed() {
    for name in STAGED {
        for kind in [Planted::Link, Planted::Fifo] {
            for delete in [false, true] {
                let rig = gate_rig(Limits::default());
                feed_canary(&rig, 1);
                wait_rows(&rig, 1);
                let token = rig.gate.preview().unwrap().token;
                let target = rig.root.path().join("victim");
                fs::write(&target, "keep").unwrap();
                let at = rig.location.dir().join(name);
                plant(kind, &at, &target);
                let result = if delete {
                    rig.gate.delete(token)
                } else {
                    rig.gate.clear(token)
                };
                let case = format!("{name} {kind:?} delete={delete}");
                let Err(Refused::Files(reason)) = &result else {
                    panic!("{case}: {result:?}");
                };
                assert!(reason.contains("not a plain file"), "{case}: {reason}");
                assert!(still_there(kind, &at), "{case}");
                assert_eq!(fs::read_to_string(&target).unwrap(), "keep", "{case}");
                assert_eq!(rows(&rig), 1, "{case}");
                assert!(rig.gate.slot().get().is_some(), "{case}");
                assert!(settings_json(&rig).contains("\"enabled\":true"), "{case}");
            }
        }
    }
}

#[test]
fn a_staged_name_that_is_not_plain_does_not_keep_the_tombstone() {
    let rig = disabled_store();
    let dir = rig.location.dir();
    fs::write(dir.join("history.db.deleting"), CANARY).unwrap();
    fs::create_dir(dir.join(STAGED[0])).unwrap();
    fs::write(dir.join(STAGED[1]), CANARY).unwrap();
    let reason = open(dir).finish_leftover().unwrap_err().to_string();
    assert!(
        reason.contains("history.db.new is not a plain file"),
        "{reason}"
    );
    assert!(!reason.contains("nothing was removed"), "{reason}");
    assert!(!dir.join("history.db.deleting").exists());
    // The staged part is refused whole.
    assert!(dir.join(STAGED[0]).is_dir());
    assert!(dir.join(STAGED[1]).exists());
}

#[test]
fn a_link_planted_just_before_staging_is_not_followed() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    let target = rig.root.path().join("victim");
    let at = rig.location.dir().join(STAGED[0]);
    // A dangling link: opening it by path would create the target.
    let result = rig.gate.clear_observed(token, &|step| {
        if step == Step::Begin {
            symlink(&target, &at).unwrap();
        }
    });
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert!(!target.exists(), "nothing was created through the link");
    assert!(still_there(Planted::Link, &at));
    assert_eq!(rows(&rig), 1);
    assert!(rig.gate.slot().get().is_some());
}

#[test]
fn recovery_still_rolls_back_a_hot_journal_when_a_staged_name_is_not_plain() {
    for name in STAGED {
        for kind in [Planted::Link, Planted::Fifo] {
            let GateRig {
                root,
                state: _,
                location,
                gate,
            } = disabled_store();
            drop(gate);
            let dir = location.dir();
            let target = root.path().join("victim");
            fs::write(&target, "keep").unwrap();
            plant(kind, &dir.join(name), &target);
            fs::write(dir.join("history.db-journal"), [7u8; 1024]).unwrap();
            super::control_files::recover(&location);
            let case = format!("{name} {kind:?}");
            assert!(
                !dir.join("history.db-journal").exists(),
                "{case}: the journal was rolled back, not skipped"
            );
            assert!(still_there(kind, &dir.join(name)), "{case}");
            assert_eq!(fs::read_to_string(&target).unwrap(), "keep", "{case}");
        }
    }
}

/// A staged file with another name for it, beside the store.
fn plant_linked_staged(dir: &Path) {
    fs::write(dir.join(STAGED[0]), CANARY).unwrap();
    fs::hard_link(dir.join(STAGED[0]), dir.join("other-name")).unwrap();
}

fn assert_staged_link_refusal(result: &Result<(), Refused>, case: &str) {
    let Err(refused @ Refused::Files(reason)) = result else {
        panic!("{case}: {result:?}");
    };
    assert!(reason.contains("staged copy"), "{case}: {reason}");
    assert!(reason.contains("another link"), "{case}: {reason}");
    let text = refused.to_string();
    assert!(!text.contains("still on disk"), "{case}: {text}");
    assert!(!text.contains("deleted history"), "{case}: {text}");
}

#[test]
fn a_linked_leftover_staged_copy_is_not_reported_as_deleted_history() {
    for delete in [false, true] {
        let rig = gate_rig(Limits::default());
        feed_canary(&rig, 1);
        wait_rows(&rig, 1);
        let token = rig.gate.preview().unwrap().token;
        plant_linked_staged(rig.location.dir());
        let result = if delete {
            rig.gate.delete(token)
        } else {
            rig.gate.clear(token)
        };
        let case = format!("delete={delete}");
        assert_staged_link_refusal(&result, &case);
        assert_eq!(rows(&rig), 1, "{case}");
        assert!(rig.gate.slot().get().is_some(), "{case}");
    }
}

#[test]
fn a_linked_staged_copy_planted_before_staging_is_not_deleted_history() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    let result = rig.gate.clear_observed(token, &|step| {
        if step == Step::Begin {
            plant_linked_staged(rig.location.dir());
        }
    });
    assert_staged_link_refusal(&result, "create");
    assert_eq!(rows(&rig), 1);
    assert!(rig.gate.slot().get().is_some());
}

#[test]
fn recovery_still_rolls_back_a_hot_journal_beside_a_linked_staged_copy() {
    let GateRig {
        root: _root,
        state: _,
        location,
        gate,
    } = disabled_store();
    drop(gate);
    let dir = location.dir();
    plant_linked_staged(dir);
    fs::write(dir.join("history.db-journal"), [7u8; 1024]).unwrap();
    super::control_files::recover(&location);
    assert!(!dir.join("history.db-journal").exists());
}

#[test]
fn a_retried_delete_unlinks_the_tombstone_despite_a_staged_name_that_is_not_plain() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    let token = rig.gate.preview().unwrap().token;
    let at = rig.location.dir().join(STAGED[0]);
    plant(Planted::Fifo, &at, &at);
    let result = rig.gate.delete(token);
    let Err(refused @ Refused::Files(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("not a plain file"), "{reason}");
    assert!(!refused.to_string().contains("nothing was removed"));
    assert!(!rig.location.dir().join("history.db.deleting").exists());
    assert!(still_there(Planted::Fifo, &at));
    assert!(rig.gate.slot().get().is_none(), "recording stays off");
}

#[test]
fn a_linked_tombstone_is_reported_before_a_staged_name_that_is_not_plain() {
    let rig = disabled_store();
    let dir = rig.location.dir();
    let tombstone = dir.join("history.db.deleting");
    fs::write(&tombstone, CANARY).unwrap();
    fs::hard_link(&tombstone, dir.join("other-name")).unwrap();
    fs::create_dir(dir.join(STAGED[0])).unwrap();
    let result = open(dir).finish_leftover();
    assert!(
        matches!(result, Err(FilesError::Linked("history.db.deleting"))),
        "{result:?}"
    );
    assert!(!tombstone.exists());
}
