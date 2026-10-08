//! The review round's cases: choices that can't be read or saved (H1),
//! "Turn all on" over a removed rule (M1), removing an edited copy (M2), a
//! refused delete, a lost daemon and a choice changed mid-pass (M5).

use super::tests::*;
use super::*;
use snitchwatch_proto::protocol::{Action, Rule};
use std::os::unix::fs::PermissionsExt;

fn all_ids() -> Vec<String> {
    entries().iter().map(|e| e.id.clone()).collect()
}

fn edited_flatpak() -> Rule {
    let mut edited = flatpak_rule();
    edited.operator.as_mut().unwrap().list[2].data = "8443".into();
    edited
}

fn unavailable(curated: &CuratedDefaults) -> (Option<String>, bool) {
    match curated.message() {
        ServerMessage::SetCuratedDefaults {
            unavailable,
            storage,
            ..
        } => (unavailable, storage.unreadable),
        other => panic!("{other:?}"),
    }
}

/// H1: a file that can't be read leaves the firewall as it is: the enabled
/// flatpak rule in the daemon is neither deleted nor taken for a removal.
#[tokio::test]
async fn unreadable_choices_change_nothing_in_the_firewall() {
    for (content, mode) in [
        ("not json", 0o600),
        (r#"{"version": 2, "choices": {}}"#, 0o666),
    ] {
        let harness = Harness::new().connect(Daemon::Accept, vec![flatpak_rule()]);
        std::fs::write(&harness.file, content).unwrap();
        std::fs::set_permissions(&harness.file, std::fs::Permissions::from_mode(mode)).unwrap();
        let curated = harness.curated();
        assert_eq!(
            unavailable(&curated),
            (Some(UNREADABLE_REASON.to_string()), true)
        );
        curated.reconcile().await;
        turn(&curated, FLATPAK, true);
        assert_eq!(curated.pass_key().version, 0, "a choice was taken");
        turn(&curated, FLATPAK, false);
        curated.try_route(ClientMessage::RemoveCuratedDefault { id: FLATPAK.into() });
        harness.resync(vec![flatpak_rule()]);
        curated.reconcile().await;
        harness.resync(Vec::new());
        curated.reconcile().await;
        assert!(harness.seen().is_empty(), "{content}: {:?}", harness.seen());
        assert_eq!(
            std::fs::read_to_string(&harness.file).unwrap(),
            content,
            "left alone"
        );
    }
}

#[tokio::test]
async fn an_inert_bridge_says_what_the_firewall_has() {
    let harness = Harness::new().connect(Daemon::Accept, vec![flatpak_rule()]);
    std::fs::write(&harness.file, "not json").unwrap();
    let curated = harness.curated();
    curated.reconcile().await;
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::InFirewall
    );
    assert_eq!(
        entry_state(&curated, "chronyc-local").status,
        EntryStatus::Unavailable
    );
}

/// H1: once a save fails, nothing more is sent, and the GUI is told.
#[tokio::test]
async fn choices_that_cant_be_saved_stop_every_command() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    let dir = harness.file.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    assert_eq!(
        unavailable(&curated),
        (Some(SAVE_FAILED_REASON.to_string()), false)
    );
    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    assert!(harness.seen().is_empty());
}

/// M1: "Turn all on" doesn't bring back a rule removed outside the page.
#[tokio::test]
async fn turn_all_on_doesnt_reinstall_a_removed_rule() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    harness.resync(Vec::new());
    curated.reconcile().await;
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::DeletedOutside
    );
    curated.try_route(ClientMessage::SetCuratedDefaults {
        ids: all_ids(),
        on: true,
    });
    curated.reconcile().await;
    let flatpak_changes = harness
        .seen()
        .iter()
        .filter(|(kind, name)| *kind == Action::ChangeRule as i32 && name == FLATPAK_RULE)
        .count();
    assert_eq!(
        flatpak_changes,
        1,
        "only the first install: {:?}",
        harness.seen()
    );
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::DeletedOutside
    );
}

/// M2: an edited copy can be removed, by its own name only, and stays
/// removed.
#[tokio::test]
async fn an_edited_copy_is_removed_only_when_asked_and_stays_removed() {
    let harness = Harness::new().connect(Daemon::Accept, vec![edited_flatpak()]);
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "an edited copy is left alone");
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::EditedByYou
    );
    for id in ["../x", "snitchwatch-default-flatpak-flathub", "unknown", ""] {
        curated.try_route(ClientMessage::RemoveCuratedDefault { id: id.into() });
    }
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());

    curated.try_route(ClientMessage::RemoveCuratedDefault { id: FLATPAK.into() });
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::DeleteRule as i32, FLATPAK_RULE.to_string())]
    );
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::DeletedOutside
    );
    harness.resync(Vec::new());
    curated.reconcile().await;
    let restarted = harness.curated();
    restarted.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "reinstalled: {:?}", harness.seen());
}

/// M2: an unedited copy isn't removed this way (turning it off does that).
#[tokio::test]
async fn removal_isnt_for_an_unedited_copy() {
    let harness = Harness::new().connect(Daemon::Accept, vec![flatpak_rule()]);
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    curated.try_route(ClientMessage::RemoveCuratedDefault { id: FLATPAK.into() });
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
}

/// M5: a refused delete says so, isn't sent again on the same daemon
/// stream, and is tried again once the daemon reconnects.
#[tokio::test]
async fn a_refused_delete_is_reported_and_retried() {
    let harness = Harness::new().connect(Daemon::RefuseDeletes, vec![flatpak_rule()]);
    let curated = harness.curated();
    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::NotRemoved);
    assert_eq!(
        state.problem.as_deref(),
        Some("The firewall service refused to remove the rule.")
    );
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "sent again: {:?}", harness.seen());
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::NotRemoved
    );
    *harness.policy.lock().unwrap() = Daemon::Accept;
    harness.resync(vec![flatpak_rule()]);
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 2, "retried: {:?}", harness.seen());
    assert_eq!(entry_state(&curated, FLATPAK).status, EntryStatus::Off);
}

async fn wait_for(status: EntryStatus, rx: &mut broadcast::Receiver<ServerMessage>) {
    let shows = |m: &ServerMessage| match m {
        ServerMessage::SetCuratedDefaults { entries, .. } => entries
            .iter()
            .any(|e| e.id == FLATPAK && e.status == status),
        _ => false,
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while !shows(&rx.recv().await.unwrap()) {}
    })
    .await
    .unwrap_or_else(|_| panic!("never {status:?}"));
}

/// M5: when the daemon goes away, the page stops claiming anything.
#[tokio::test]
async fn a_lost_daemon_turns_every_status_to_waiting() {
    let mut harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let mut rx = harness.broadcast.subscribe();
    let curated = harness.curated();
    let worker = curated.spawn(harness.rules.synced());
    turn(&curated, FLATPAK, true);
    wait_for(EntryStatus::Installed, &mut rx).await;
    drop(harness.stream.take());
    wait_for(EntryStatus::Waiting, &mut rx).await;
    worker.abort();
}

/// M5 and the review's LOW: a choice changed while an earlier command of
/// the pass waits is honoured before the next command.
#[tokio::test]
async fn a_choice_changed_mid_pass_is_honoured() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, "chronyc-local", true);
    turn(&curated, FLATPAK, true);
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    let pass = tokio::spawn({
        let curated = curated.clone();
        async move { curated.reconcile().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.seen().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    turn(&curated, FLATPAK, false);
    gate.notify_one();
    pass.await.unwrap();
    assert_eq!(
        harness.seen(),
        [(
            Action::ChangeRule as i32,
            "snitchwatch-default-chronyc-local".to_string()
        )]
    );
}
