//! PR #105 re-review: no command is re-sent in a loop, a first run leaves
//! the firewall alone, and each inert guard holds on its own (LOW-2).

use super::tests::*;
use super::*;
use snitchwatch_proto::protocol::{Action, Rule};
use std::os::unix::fs::PermissionsExt;

/// The real worker, left running against `harness` for `ms`.
async fn run_worker_for(harness: &Harness, curated: &CuratedDefaults, ms: u64) {
    let worker = curated.spawn(harness.rules.synced());
    tokio::time::sleep(Duration::from_millis(ms)).await;
    worker.abort();
}

fn edited(id: &str) -> Rule {
    let mut rule = entries().iter().find(|e| e.id == id).unwrap().rule();
    let port = &mut rule.operator.as_mut().unwrap().list[2];
    port.data = "8443".into();
    rule
}

#[tokio::test]
async fn a_refused_install_is_sent_once_not_in_a_loop() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    run_worker_for(&harness, &curated, 300).await;
    assert_eq!(harness.seen().len(), 1, "{} sends", harness.seen().len());
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::NotInstalled
    );
}

#[tokio::test]
async fn a_refused_delete_is_sent_once_not_in_a_loop() {
    let harness = Harness::new().connect(Daemon::RefuseDeletes, vec![flatpak_rule()]);
    let curated = harness.curated();
    turn(&curated, FLATPAK, false);
    run_worker_for(&harness, &curated, 300).await;
    assert_eq!(harness.seen().len(), 1, "{} sends", harness.seen().len());
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::NotRemoved
    );
}

/// A choice changed after a failure tries again, once.
#[tokio::test]
async fn a_new_choice_tries_a_failed_command_again() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1);
    turn(&curated, FLATPAK, false);
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 2);
}

/// Re-review M1: a first run (or a choices file moved away) deletes
/// nothing already in the firewall; the switch shows the rule active.
#[tokio::test]
async fn a_first_run_sends_nothing_for_a_copy_already_there() {
    let harness = Harness::new().connect(Daemon::Accept, vec![flatpak_rule()]);
    let curated = harness.curated();
    run_worker_for(&harness, &curated, 200).await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::InFirewall);
    assert!(state.on, "the switch shows the rule active");
}

/// LOW-2: a save that fails after an `OK` stops the rest of the pass
/// (`still_wanted`'s inert check).
#[tokio::test]
async fn a_failed_save_after_an_ok_skips_the_next_install() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, "chronyc-local", true);
    turn(&curated, FLATPAK, true);
    curated.save_if_changed().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    let pass = tokio::spawn({
        let curated = curated.clone();
        async move { curated.reconcile().await }
    });
    wait_until(|| !harness.seen().is_empty()).await;
    let dir = harness.file.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    gate.notify_one();
    pass.await.unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(harness.seen().len(), 1, "{:?}", harness.seen());
    assert!(curated.is_inert());
}

/// LOW-2: the same for removals (the removal loop's inert break).
#[tokio::test]
async fn a_failed_save_after_a_removal_skips_the_next_removal() {
    let harness = Harness::new().connect(
        Daemon::Accept,
        vec![edited(FLATPAK), edited("chronyc-local")],
    );
    let curated = harness.curated();
    curated.reconcile().await;
    for id in [FLATPAK, "chronyc-local"] {
        curated.try_route(ClientMessage::RemoveCuratedDefault { id: id.into() });
    }
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    let pass = tokio::spawn({
        let curated = curated.clone();
        async move { curated.reconcile().await }
    });
    wait_until(|| !harness.seen().is_empty()).await;
    let dir = harness.file.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    gate.notify_one();
    pass.await.unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(harness.seen().len(), 1, "{:?}", harness.seen());
    assert_eq!(harness.seen()[0].0, Action::DeleteRule as i32);
}

/// LOW-2: an inert bridge doesn't even queue a removal of an edited copy.
#[tokio::test]
async fn an_inert_bridge_queues_no_removal() {
    let harness = Harness::new().connect(Daemon::Accept, vec![edited(FLATPAK)]);
    std::fs::write(&harness.file, "not json").unwrap();
    let curated = harness.curated();
    curated.try_route(ClientMessage::RemoveCuratedDefault { id: FLATPAK.into() });
    assert!(
        !curated.pass_key().removals,
        "a removal was queued while inert"
    );
    curated.reconcile().await;
    assert!(harness.seen().is_empty());
}

/// LOW-4: a removal asked for under a list that is then withdrawn isn't
/// carried over to the next list.
#[tokio::test]
async fn a_withdrawn_list_drops_queued_removals() {
    let mut harness = Harness::new().connect(Daemon::Accept, vec![edited(FLATPAK)]);
    let curated = harness.curated();
    curated.reconcile().await;
    curated.try_route(ClientMessage::RemoveCuratedDefault { id: FLATPAK.into() });
    drop(harness.stream.take());
    curated.reconcile().await;
    assert!(!curated.pass_key().removals);
}

async fn wait_until(done: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out");
}
