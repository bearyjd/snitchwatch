//! #444: a clear builds the new store, with what it keeps, beside the old one
//! and replaces it in one rename. A failure before the rename leaves the old
//! store whole; from the rename on, the clear is never reported as undone.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use super::control::{Limits, Refused};
use super::control_clear_tests::{feed_canary, rows, wait_rows};
use super::control_files::{with_fault, Fault, Step};
use super::control_preview::TokenId;
use super::control_support::count;
use super::control_tests::{feed, gate_rig, GateRig};

const STAGED: &str = "history.db.new";
const KEPT: &str = "SELECT COUNT(*) FROM events WHERE dst_host = 'h0.example'";

fn chmod(rig: &GateRig, mode: u32) {
    fs::set_permissions(rig.location.dir(), fs::Permissions::from_mode(mode)).unwrap();
}

/// A rig with one previewed row and one more accepted after the preview.
fn previewed_with_one_to_carry() -> (GateRig, TokenId) {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    feed(&rig, 1);
    wait_rows(&rig, 2);
    (rig, token)
}

fn staged_left(rig: &GateRig) -> Vec<String> {
    let mut left: Vec<String> = fs::read_dir(rig.location.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(STAGED))
        .collect();
    left.sort();
    left
}

/// A plain refusal is true: the old store is whole and recording goes on.
fn assert_old_store_whole(rig: &GateRig, case: &str) {
    assert_eq!(rows(rig), 2, "{case}: both rows are still there");
    assert!(rig.gate.slot().get().is_some(), "{case}: recording");
    assert_eq!(staged_left(rig), Vec::<String>::new(), "{case}");
}

/// The new store is in place, holding exactly the row that was kept.
fn assert_replaced(rig: &GateRig) {
    assert_eq!(rows(rig), 1, "only the carried row");
    assert_eq!(count(&rig.location, KEPT), 1);
    assert_eq!(staged_left(rig), Vec::<String>::new());
}

/// Runs `sql` on the staged store from a second connection.
fn on_staged(rig: &GateRig, sql: &str) {
    let db = rusqlite::Connection::open(rig.location.dir().join(STAGED)).unwrap();
    db.execute_batch(sql).unwrap();
}

#[test]
fn a_clear_stages_the_carry_then_replaces_the_store() {
    let (rig, token) = previewed_with_one_to_carry();
    let steps = std::cell::RefCell::new(Vec::new());
    rig.gate
        .clear_observed(token, &|at| steps.borrow_mut().push(at))
        .unwrap();
    assert_eq!(
        steps.into_inner(),
        [
            Step::Begin,
            Step::Created,
            Step::Carried,
            Step::Staged,
            Step::Replaced
        ]
    );
    assert_replaced(&rig);
    assert!(rig.gate.slot().get().is_some());
    let mode = fs::metadata(rig.location.db_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o600);
}

#[test]
fn a_full_disk_while_staging_leaves_the_old_store_whole_and_recording() {
    let (rig, token) = previewed_with_one_to_carry();
    // The insert of the carry fails as a full disk would.
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Created {
            on_staged(
                &rig,
                "CREATE TRIGGER full BEFORE INSERT ON events BEGIN \
                 SELECT RAISE(ABORT, 'database or disk is full'); END;",
            );
        }
    });
    let Err(Refused::Unavailable(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("disk is full"), "{reason}");
    assert_old_store_whole(&rig, "full disk");
}

#[test]
fn a_staged_file_that_cannot_be_synced_is_discarded() {
    let (rig, token) = previewed_with_one_to_carry();
    let result = with_fault(Fault::StagedSync, || rig.gate.clear(token));
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert_old_store_whole(&rig, "staged sync");
}

#[test]
fn a_staged_store_that_does_not_hold_the_carry_is_refused() {
    for sql in [
        "INSERT INTO events (ts_ms, action, answered_by) VALUES (1, 'allow', 'rule');",
        "DELETE FROM events;",
        "DELETE FROM rules_seen;",
        "INSERT INTO rules_seen (rule_hash, snapshot, first_seen_ms) \
         VALUES (zeroblob(16), '{}', 1);",
        "PRAGMA user_version = 3;",
    ] {
        let (rig, token) = previewed_with_one_to_carry();
        let result = rig.gate.clear_observed(token, &|at| {
            if at == Step::Carried {
                on_staged(&rig, sql);
            }
        });
        assert!(
            matches!(result, Err(Refused::Unavailable(_))),
            "{sql}: {result:?}"
        );
        assert_old_store_whole(&rig, sql);
    }
}

/// Positive control for the `rules_seen` cases above: the carry has a rule.
#[test]
fn a_clear_carries_the_rule_of_a_kept_row() {
    let (rig, token) = previewed_with_one_to_carry();
    rig.gate.clear(token).unwrap();
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM rules_seen"), 1);
}

#[test]
fn a_swapped_staged_file_is_refused() {
    for step in [Step::Created, Step::Carried, Step::Staged] {
        let (rig, token) = previewed_with_one_to_carry();
        let dir = rig.location.dir().to_owned();
        // The same bytes under the same name, but another file.
        let result = rig.gate.clear_observed(token, &|at| {
            if at == step {
                fs::copy(dir.join(STAGED), dir.join("copy")).unwrap();
                fs::rename(dir.join("copy"), dir.join(STAGED)).unwrap();
            }
        });
        assert!(result.is_err(), "{step:?}: {result:?}");
        assert!(
            !matches!(result, Err(Refused::RollbackFailed { .. })),
            "{step:?}: {result:?}"
        );
        assert_old_store_whole(&rig, &format!("{step:?}"));
    }
}

#[test]
fn a_staged_file_with_a_second_link_is_refused() {
    let (rig, token) = previewed_with_one_to_carry();
    let dir = rig.location.dir().to_owned();
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Staged {
            fs::hard_link(dir.join(STAGED), dir.join("other-name")).unwrap();
        }
    });
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert_old_store_whole(&rig, "linked staged file");
}

#[test]
fn a_sidecar_present_at_replace_time_is_refused() {
    for name in [
        "history.db-journal",
        "history.db-wal",
        "history.db-shm",
        "history.db.new-journal",
        "history.db.new-wal",
        "history.db.new-shm",
    ] {
        let (rig, token) = previewed_with_one_to_carry();
        let dir = rig.location.dir().to_owned();
        let result = rig.gate.clear_observed(token, &|at| {
            if at == Step::Staged {
                fs::write(dir.join(name), b"x").unwrap();
            }
        });
        let Err(Refused::Files(reason)) = &result else {
            panic!("{name}: {result:?}");
        };
        assert!(reason.contains(name), "{reason}");
        assert_old_store_whole(&rig, name);
    }
}

#[test]
fn a_tombstone_present_at_replace_time_is_refused() {
    let (rig, token) = previewed_with_one_to_carry();
    let tombstone = rig.location.dir().join("history.db.deleting");
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Staged {
            fs::write(&tombstone, b"x").unwrap();
        }
    });
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert!(tombstone.exists(), "not removed by this clear");
    assert_old_store_whole(&rig, "tombstone");
}

#[test]
fn a_live_name_that_is_not_a_plain_store_at_replace_time_is_not_replaced() {
    for link in [false, true] {
        let (rig, token) = previewed_with_one_to_carry();
        let dir = rig.location.dir().to_owned();
        let moved = dir.join("moved.db");
        let result = rig.gate.clear_observed(token, &|at| {
            if at == Step::Staged {
                fs::rename(dir.join("history.db"), &moved).unwrap();
                if link {
                    symlink(&moved, dir.join("history.db")).unwrap();
                }
            }
        });
        // Recording cannot restart on a link, so the refusal may say so.
        assert!(
            matches!(
                result,
                Err(Refused::Files(_) | Refused::RollbackFailed { .. })
            ),
            "{link}: {result:?}"
        );
        assert_eq!(staged_left(&rig), Vec::<String>::new(), "{link}");
        let db = rusqlite::Connection::open(&moved).unwrap();
        let old: i64 = db
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old, 2, "{link}: the old store is untouched");
        let live = fs::symlink_metadata(dir.join("history.db"));
        if link {
            assert!(live.unwrap().file_type().is_symlink(), "left as it was");
        }
    }
}

#[test]
fn a_failed_rename_with_the_old_store_in_place_is_a_plain_refusal() {
    let (rig, token) = previewed_with_one_to_carry();
    let result = with_fault(Fault::Rename, || rig.gate.clear(token));
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert_old_store_whole(&rig, "rename");
}

#[test]
fn a_rename_that_reports_a_failure_but_happened_counts_as_cleared() {
    let (rig, token) = previewed_with_one_to_carry();
    with_fault(Fault::RenameReportedFailed, || rig.gate.clear(token)).unwrap();
    assert_replaced(&rig);
    assert!(rig.gate.slot().get().is_some());
}

#[test]
fn an_unknown_replace_outcome_is_reported_as_neither_cleared_nor_unchanged() {
    let (rig, token) = previewed_with_one_to_carry();
    let result = with_fault(Fault::OutcomeUnknown, || rig.gate.clear(token));
    let Err(Refused::ClearOutcomeUnknown(reason)) = &result else {
        panic!("{result:?}");
    };
    let shown = result.as_ref().unwrap_err().to_string();
    assert!(shown.contains("cannot tell"), "{shown} / {reason}");
    assert!(reason.contains("recording stays off"), "{reason}");
    assert!(rig.gate.slot().get().is_none());
}

#[test]
fn a_failed_directory_sync_says_the_clear_may_not_survive_and_stops_recording() {
    let (rig, token) = previewed_with_one_to_carry();
    let result = with_fault(Fault::DirSync, || rig.gate.clear(token));
    let Err(Refused::ClearedNotDurable(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("recording stays off"), "{reason}");
    assert!(rig.gate.slot().get().is_none());
    assert_replaced(&rig);
}

#[test]
fn recording_that_cannot_resume_after_the_replace_says_the_history_was_cleared() {
    let (rig, token) = previewed_with_one_to_carry();
    // A directory that is not exactly 0700 refuses to open the store.
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Replaced {
            chmod(&rig, 0o500);
        }
    });
    chmod(&rig, 0o700);
    let Err(refused @ Refused::ClearedNotResumed { kept: 1, .. }) = &result else {
        panic!("{result:?}");
    };
    let text = refused.to_string();
    assert!(text.contains("history was cleared"), "{text}");
    assert!(text.contains("1 rows accepted since the preview"), "{text}");
    assert!(rig.gate.slot().get().is_none());
    assert_replaced(&rig);
}

#[test]
fn a_clear_that_is_not_recording_still_keeps_the_later_rows() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    rig.gate.disable().unwrap();
    let token = rig.gate.preview().unwrap().token;
    let db = rusqlite::Connection::open(rig.location.db_path()).unwrap();
    db.execute(
        "INSERT INTO events (ts_ms, dst_host, action, answered_by) \
         VALUES (1, 'h0.example', 'allow', 'rule')",
        [],
    )
    .unwrap();
    drop(db);
    rig.gate.clear(token).unwrap();
    assert_replaced(&rig);
    assert!(rig.gate.slot().get().is_none(), "recording stays off");
}

/// A recording rig whose `history.db` was unlinked from outside, previewed.
fn previewed_without_a_store() -> (GateRig, TokenId) {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    fs::remove_file(rig.location.db_path()).unwrap();
    let token = rig.gate.preview().unwrap().token;
    (rig, token)
}

#[test]
fn a_clear_while_recording_with_no_store_makes_a_new_one() {
    let (rig, token) = previewed_without_a_store();
    let steps = std::cell::RefCell::new(Vec::new());
    rig.gate
        .clear_observed(token, &|at| steps.borrow_mut().push(at))
        .unwrap();
    assert_eq!(steps.into_inner().last(), Some(&Step::Replaced));
    assert_eq!(rows(&rig), 0);
    assert_eq!(staged_left(&rig), Vec::<String>::new());
    assert!(rig.gate.slot().get().is_some(), "recording");
    feed(&rig, 1);
    wait_rows(&rig, 1);
}

#[test]
fn what_appears_where_there_was_no_store_is_never_replaced() {
    for kind in ["file", "link", "directory"] {
        let (rig, token) = previewed_without_a_store();
        let live = rig.location.db_path();
        let target = rig.root.path().join("victim");
        fs::write(&target, "keep").unwrap();
        let result = rig.gate.clear_observed(token, &|at| {
            if at == Step::Staged {
                match kind {
                    "file" => fs::write(&live, "planted").unwrap(),
                    "link" => symlink(&target, &live).unwrap(),
                    _ => fs::create_dir(&live).unwrap(),
                }
            }
        });
        // Recording cannot start on what was planted, and the refusal says so.
        let Err(Refused::RollbackFailed { refused, .. }) = &result else {
            panic!("{kind}: {result:?}");
        };
        assert!(refused.contains("File exists"), "{kind}: {refused}");
        match kind {
            "file" => assert_eq!(fs::read_to_string(&live).unwrap(), "planted"),
            "link" => assert_eq!(fs::read_link(&live).unwrap(), target),
            _ => assert!(live.is_dir()),
        }
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep", "{kind}");
        assert_eq!(staged_left(&rig), Vec::<String>::new(), "{kind}");
    }
}

#[test]
fn a_failed_rename_with_no_store_is_a_plain_refusal() {
    let (rig, token) = previewed_without_a_store();
    let result = with_fault(Fault::Rename, || rig.gate.clear(token));
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert_eq!(staged_left(&rig), Vec::<String>::new());
    assert!(rig.gate.slot().get().is_some(), "recording");
}

#[test]
fn a_rename_onto_no_store_that_reports_a_failure_but_happened_counts() {
    let (rig, token) = previewed_without_a_store();
    with_fault(Fault::RenameReportedFailed, || rig.gate.clear(token)).unwrap();
    assert!(rig.gate.slot().get().is_some(), "recording");
}
