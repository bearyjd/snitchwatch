//! PR #105 re-review: no command is re-sent in a loop, a first run leaves
//! the firewall alone, and each inert guard holds on its own (LOW-2).

use super::tests::*;
use super::*;
use snitchwatch_proto::protocol::{Action, Rule};
use std::os::unix::fs::PermissionsExt;

/// How long a hot loop gets to show itself once the worker has settled:
/// unbounded re-sending ran thousands of commands in this time (PR #105
/// re-review), while a correct worker sends nothing more.
const SETTLE: Duration = Duration::from_millis(400);

/// Wait, by observation rather than wall time, until `done` holds; a slow
/// runner only makes this take longer.
async fn eventually(what: &str, done: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn passes(curated: &CuratedDefaults) -> u64 {
    curated
        .inner
        .passes
        .load(std::sync::atomic::Ordering::SeqCst)
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
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first install", || !harness.seen().is_empty()).await;
    tokio::time::sleep(SETTLE).await;
    worker.abort();
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
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first delete", || !harness.seen().is_empty()).await;
    tokio::time::sleep(SETTLE).await;
    worker.abort();
    assert_eq!(harness.seen().len(), 1, "{} sends", harness.seen().len());
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::OffFileLeft
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
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first pass", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::InFirewall
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    worker.abort();
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    assert!(
        entry_state(&curated, FLATPAK).on,
        "the switch shows the rule active"
    );
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
    let queued = !lock(&curated.inner.state).removals.is_empty();
    assert!(!queued, "a removal was queued while inert");
    assert_eq!(
        curated.pass_key().requests,
        0,
        "a request was taken while inert"
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
    assert!(lock(&curated.inner.state).removals.is_empty());
}

async fn wait_until(done: impl Fn() -> bool) {
    eventually("the first command", done).await;
}

/// The pass gate: rule-list broadcasts that change nothing a pass reads run
/// no pass.
#[tokio::test]
async fn broadcasts_that_change_nothing_run_no_pass() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    let worker = curated.spawn(harness.rules.synced());
    eventually("the first pass", || passes(&curated) >= 1).await;
    tokio::time::sleep(SETTLE).await;
    let before = passes(&curated);
    for _ in 0..50 {
        let _ = harness
            .broadcast
            .send(ServerMessage::SetRules { rules: Vec::new() });
        tokio::task::yield_now().await;
    }
    // The worker read them all (it is the broadcast's only receiver).
    eventually("the worker to read the broadcasts", || {
        harness.broadcast.is_empty()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    worker.abort();
    let after = passes(&curated);
    assert_eq!(after, before, "{} passes for nothing", after - before);
}

fn sent(action: Action) -> (i32, String) {
    (action as i32, FLATPAK_RULE.to_string())
}

/// Wait for a pass that starts after now.
async fn another_pass(curated: &CuratedDefaults, ask: impl FnOnce()) {
    let before = passes(curated);
    ask();
    eventually("another pass", || passes(curated) > before).await;
}

/// PR #119 review M1: stock `replaceUserRule` takes a rule into memory
/// before `Save` fails, so a refused install may apply though the list
/// lacks it. Off deletes it, once: more passes send nothing more (no hot
/// loop), and on again installs once.
#[tokio::test]
async fn off_after_a_refused_install_deletes_it_once() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    let worker = curated.spawn(harness.rules.synced());
    eventually("the refused install", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::NotInstalled
    })
    .await;
    turn(&curated, FLATPAK, false);
    eventually("the delete", || harness.seen().len() >= 2).await;
    eventually("off, file left", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::OffFileLeft
    })
    .await;
    for _ in 0..3 {
        another_pass(&curated, || turn(&curated, FLATPAK, false)).await;
    }
    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        harness.seen(),
        vec![sent(Action::ChangeRule), sent(Action::DeleteRule)]
    );

    *harness.policy.lock().unwrap() = Daemon::Accept;
    turn(&curated, FLATPAK, true);
    eventually("installed", || {
        entry_state(&curated, FLATPAK).status == EntryStatus::Installed
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    worker.abort();
    assert_eq!(
        harness.seen(),
        vec![
            sent(Action::ChangeRule),
            sent(Action::DeleteRule),
            sent(Action::ChangeRule)
        ]
    );
}
