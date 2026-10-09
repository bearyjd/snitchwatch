//! PR #105 re-review 2: every request gets a pass, a reconnect ends a
//! stale pass, a refused removal keeps its status, and the smaller guards.

use super::state::{save_in_order, send_problem, SaveJob};
use super::tests::*;
use super::*;
use crate::curated::store::Choices;
use crate::daemon_commands::SendError;
use snitchwatch_proto::protocol::{Action, Rule};

const B: &str = "chronyc-local";

fn rule_of(id: &str) -> Rule {
    entries().iter().find(|e| e.id == id).unwrap().rule()
}

fn edited(id: &str) -> Rule {
    let mut rule = rule_of(id);
    rule.operator.as_mut().unwrap().list[2].data = "8443".into();
    rule
}

async fn eventually(what: &str, done: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn remove(curated: &CuratedDefaults, id: &str) {
    curated.try_route(ClientMessage::RemoveCuratedDefault { id: id.into() });
}

/// HIGH: a second Remove asked while a first, refused one is in flight is
/// sent, not dropped by the pass gate.
#[tokio::test]
async fn a_removal_asked_during_a_refused_one_is_sent() {
    let harness = Harness::new().connect(Daemon::RefuseDeletes, vec![edited(FLATPAK), edited(B)]);
    let curated = harness.curated();
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first pass", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::EditedByYou
    })
    .await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    remove(&curated, FLATPAK);
    eventually("the first delete", || harness.seen().len() == 1).await;
    remove(&curated, B);
    gate.notify_one();
    eventually("the second delete", || harness.seen().len() == 2).await;
    worker.abort();
    assert_eq!(
        harness.seen()[1],
        (
            Action::DeleteRule as i32,
            "snitchwatch-default-chronyc-local".to_string()
        )
    );
}

/// LOW 1: asking "on" again for an entry already on tries a failed install
/// again, once.
#[tokio::test]
async fn asking_again_for_a_failed_install_runs_a_pass() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first install", || harness.seen().len() == 1).await;
    turn(&curated, FLATPAK, true);
    eventually("the second install", || harness.seen().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    worker.abort();
    assert_eq!(harness.seen().len(), 2);
}

/// M1: a reconnect mid-pass ends the pass; the new list's edited copy is
/// not deleted from the old plan.
#[tokio::test]
async fn a_reconnect_mid_pass_doesnt_delete_a_copy_edited_meanwhile() {
    let harness = Harness::new().connect(Daemon::Accept, vec![rule_of(FLATPAK), rule_of(B)]);
    let curated = harness.curated();
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    curated.try_route(ClientMessage::SetCuratedDefaults {
        ids: vec![FLATPAK.into(), B.into()],
        on: false,
    });
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first delete", || harness.seen().len() == 1).await;
    let first = harness.seen()[0].1.clone();
    let (edited_id, kept_id) = if first.ends_with(B) {
        (FLATPAK, B)
    } else {
        (B, FLATPAK)
    };
    // The daemon reconnects with the other entry edited.
    harness.resync(vec![edited(edited_id), rule_of(kept_id)]);
    gate.notify_one();
    eventually("the edited status", || {
        entry_state(&curated, edited_id).status == EntryStatus::EditedByYou
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    worker.abort();
    assert!(
        !harness
            .seen()
            .iter()
            .any(|(kind, name)| *kind == Action::DeleteRule as i32 && name.ends_with(edited_id)),
        "the edited copy was deleted: {:?}",
        harness.seen()
    );
}

/// M2, tower r12: a refused removal keeps its status. The daemon dropped
/// the copy before failing on its file, so it is removed as asked; the
/// status says its file is left, and keeps saying so through an unrelated
/// pass.
#[tokio::test]
async fn a_refused_removal_keeps_its_status() {
    let harness = Harness::new().connect(Daemon::RefuseDeletes, vec![edited(FLATPAK)]);
    let curated = harness.curated();
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first pass", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::EditedByYou
    })
    .await;
    remove(&curated, FLATPAK);
    eventually("off, file left", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::OffFileLeft
    })
    .await;
    // Another pass (an unrelated choice, seen by its install) doesn't wipe
    // it.
    turn(&curated, B, true);
    eventually("the unrelated pass", || {
        entry_state(&curated, B).status == EntryStatus::Installed
    })
    .await;
    worker.abort();
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::OffFileLeft);
    assert_eq!(state.problem, None);
    assert_eq!(harness.seen().len(), 2, "{:?}", harness.seen());
}

/// Tower r12: a refused removal of an edited copy, with the entry on, is a
/// removal the user asked for like any other: the canonical rule isn't
/// installed in its place (as after a removal the daemon confirmed).
#[tokio::test]
async fn a_refused_removal_of_an_edited_copy_is_not_replaced_by_an_install() {
    let harness = Harness::new().connect(Daemon::RefuseDeletes, vec![edited(FLATPAK)]);
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first pass", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::EditedByYou
    })
    .await;
    remove(&curated, FLATPAK);
    eventually("deleted outside", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::DeletedOutside
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    worker.abort();
    assert_eq!(
        harness.seen(),
        vec![(
            Action::DeleteRule as i32,
            "snitchwatch-default-flatpak-flathub".to_string()
        )]
    );
}

/// M3: a first-run copy turned off on the Rules page reads as off.
#[tokio::test]
async fn an_undecided_copy_turned_off_shows_its_switch_off() {
    let off = Rule {
        enabled: false,
        ..flatpak_rule()
    };
    let harness = Harness::new().connect(Daemon::Accept, vec![off]);
    let curated = harness.curated();
    curated.reconcile().await;
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::InFirewallButOff);
    assert!(!state.on);
    assert!(harness.seen().is_empty());
}

/// LOW 2: two saves out of order leave the newer one on disk.
#[test]
fn an_older_save_never_replaces_a_newer_one() {
    let harness = Harness::new();
    let written = Mutex::new(0);
    let newer = Choices::default().enable(FLATPAK);
    let job = |choices: &Choices, version| SaveJob {
        file: harness.file.clone(),
        choices: choices.clone(),
        version,
    };
    save_in_order(&written, job(&newer, 2)).unwrap();
    save_in_order(&written, job(&Choices::default(), 1)).unwrap();
    assert_eq!(store::load(&harness.file).unwrap(), Some(newer));
}

/// LOW 2: an install isn't sent once the rule has appeared in the live
/// list (here while an earlier install of the pass was waiting).
#[tokio::test]
async fn an_install_isnt_sent_once_the_rule_appeared() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, B, true);
    turn(&curated, FLATPAK, true);
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    let pass = tokio::spawn({
        let curated = curated.clone();
        async move { curated.reconcile().await }
    });
    eventually("the first install", || harness.seen().len() == 1).await;
    let first = harness.seen()[0].1.clone();
    let other = if first.ends_with(B) { FLATPAK } else { B };
    harness.rules.upsert(rule_of(other));
    gate.notify_one();
    pass.await.unwrap();
    assert_eq!(harness.seen().len(), 1, "{:?}", harness.seen());
}

/// LOW 3: a full queue isn't remembered as a failure; the rest are.
#[test]
fn only_a_full_queue_may_be_tried_again_at_once() {
    assert!(!send_problem(SendError::NotQueued).sticky);
    assert!(send_problem(SendError::NoDaemon).sticky);
    assert!(send_problem(SendError::RefusedOperator).sticky);
}

/// M1, for removals: a reconnect mid-pass ends it; a removal not yet sent
/// is decided against the new list: sent if the copy is still edited, not
/// if it is no longer.
#[tokio::test]
async fn a_reconnect_mid_pass_redecides_the_removals_not_yet_sent() {
    for still_edited in [true, false] {
        let harness = Harness::new().connect(Daemon::Accept, vec![edited(FLATPAK), edited(B)]);
        let curated = harness.curated();
        curated.reconcile().await;
        remove(&curated, FLATPAK);
        remove(&curated, B);
        let gate = Arc::new(tokio::sync::Notify::new());
        *harness.hold.lock().unwrap() = Some(gate.clone());
        let worker = curated.spawn(harness.rules.synced());
        eventually("the first delete", || harness.seen().len() == 1).await;
        let first = harness.seen()[0].1.clone();
        let other = if first.ends_with(B) { FLATPAK } else { B };
        let others_copy = if still_edited {
            edited(other)
        } else {
            rule_of(other)
        };
        harness.resync(vec![others_copy]);
        gate.notify_one();
        if still_edited {
            eventually("the second delete", || harness.seen().len() == 2).await;
        } else {
            // The follow-up pass has seen the new list (the unedited copy
            // reads "In the firewall") and taken the queued removal.
            eventually("the follow-up pass", || {
                entry_state(&curated, other).status == EntryStatus::InFirewall
                    && lock(&curated.inner.state).removals.is_empty()
            })
            .await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(harness.seen().len(), 1, "{:?}", harness.seen());
        }
        worker.abort();
    }
}

/// Re-review 3, B3: a Remove that found the command queue full reads
/// "busy" and is sent at the next change of the pass's inputs.
#[tokio::test]
async fn a_busy_removal_is_sent_once_the_queue_drains() {
    let harness = Harness::new().connect(Daemon::Accept, vec![edited(FLATPAK)]);
    let curated = harness.curated();
    curated.reconcile().await;
    // The daemon holds the first command; the rest fill the queue.
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    let filler = |i: usize| {
        let mut rule = rule_of(B);
        rule.name = format!("user-filler-{i}");
        rule.description = String::new();
        snitchwatch_proto::protocol::Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![rule],
            ..Default::default()
        }
    };
    harness.commands.send(filler(0)).unwrap();
    eventually("the daemon to hold the first", || harness.seen().len() == 1).await;
    let mut queued = 1;
    while harness.commands.send(filler(queued)).is_ok() {
        queued += 1;
    }
    remove(&curated, FLATPAK);
    curated.reconcile().await;
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::NotRemoved);
    assert!(state.problem.unwrap().contains("busy"));
    // The queue drains; the next pass (run here by hand) asks again.
    gate.notify_one();
    eventually("the queue to drain", || harness.seen().len() == queued).await;
    curated.reconcile().await;
    let deletes: Vec<_> = harness
        .seen()
        .into_iter()
        .filter(|(kind, _)| *kind == Action::DeleteRule as i32)
        .collect();
    assert_eq!(
        deletes,
        [(Action::DeleteRule as i32, FLATPAK_RULE.to_string())]
    );
}
