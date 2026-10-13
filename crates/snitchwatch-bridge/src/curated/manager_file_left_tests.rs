//! #120 items 18-20: a file a refused delete may have left is remembered
//! across bridge restarts, honoured by a confirmed delete, and forgotten
//! for a rule that never had one.

use super::tests::*;
use super::*;
use snitchwatch_proto::protocol::Action;

fn status(curated: &CuratedDefaults) -> EntryStatus {
    entry_state(curated, FLATPAK).status
}

fn marked(harness: &Harness) -> bool {
    harness
        .rules
        .cache()
        .lock()
        .unwrap()
        .files_left()
        .contains(FLATPAK_RULE)
}

/// The listed rule turned off, its delete refused: its file may remain.
async fn refused_delete() -> (Harness, CuratedDefaults) {
    let harness = Harness::new().connect(Daemon::RefuseDeletes, vec![flatpak_rule()]);
    let curated = harness.curated();
    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    assert_eq!(status(&curated), EntryStatus::OffFileLeft);
    (harness, curated)
}

/// The bridge restarts (a new rules cache, with no marker) on the saved
/// choices, and the daemon, still without the rule, answers as `daemon`.
async fn restarted(before: &Harness, daemon: Daemon) -> (Harness, CuratedDefaults) {
    let harness = Harness::new().connect(daemon, Vec::new());
    std::fs::copy(&before.file, &harness.file).unwrap();
    let curated = harness.curated();
    curated.reconcile().await;
    (harness, curated)
}

/// Item 18: the warning survives a bridge restart, and sends nothing.
#[tokio::test]
async fn a_file_left_is_still_shown_after_a_bridge_restart() {
    let (before, _) = refused_delete().await;
    let (harness, curated) = restarted(&before, Daemon::Accept).await;
    assert_eq!(status(&curated), EntryStatus::OffFileLeft);
    assert!(harness.seen().is_empty());
}

/// Item 18 (the #15 case): after a restart, on again is refused and so is
/// the delete that follows (#130). The file noted in the earlier run may
/// still be there, so the entry doesn't read plain `Off`.
#[tokio::test]
async fn a_file_left_in_an_earlier_run_still_counts_after_a_refused_install() {
    let (before, _) = refused_delete().await;
    let (harness, curated) = restarted(&before, Daemon::Refuse).await;
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [
            (Action::ChangeRule as i32, FLATPAK_RULE.into()),
            (Action::DeleteRule as i32, FLATPAK_RULE.into()),
        ]
    );
    assert_eq!(status(&curated), EntryStatus::OffFileLeft);
}

/// Item 18: an install the daemon confirms rewrote the file: the bit goes.
#[tokio::test]
async fn a_confirmed_install_forgets_the_saved_bit() {
    let (before, _) = refused_delete().await;
    let (harness, curated) = restarted(&before, Daemon::Accept).await;
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert_eq!(status(&curated), EntryStatus::Installed);
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(saved.files_left.is_empty(), "{saved:?}");
}

/// Item 19: on a daemon that rolls a refused install back (tower #109), the
/// delete that follows answers `OK` without touching the file noted before:
/// the entry still says the file may remain.
#[tokio::test]
async fn an_ok_delete_of_an_unlisted_rule_keeps_the_file_warning() {
    let (harness, curated) = refused_delete().await;
    *harness.policy.lock().unwrap() = Daemon::Refuse;
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    *harness.policy.lock().unwrap() = Daemon::Accept;
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 3, "{:?}", harness.seen());
    assert_eq!(status(&curated), EntryStatus::OffFileLeft);
    assert!(marked(&harness));
}

/// Item 20: the delete of a refused install that wrote no file times out,
/// and its late `ERROR` marks a file left that can't exist. The next pass
/// forgets it: the entry reads `Off`, and the rule list's count drops.
#[tokio::test(start_paused = true)]
async fn a_late_error_to_the_delete_of_a_rule_with_no_file_leaves_no_warning() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    curated.reconcile().await;
    assert_eq!(status(&curated), EntryStatus::NotRemoved);
    gate.notify_one();
    for _ in 0..100 {
        if marked(&harness) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(marked(&harness), "the late ERROR marks a file left");
    curated.reconcile().await;
    assert!(!marked(&harness));
    assert_ne!(status(&curated), EntryStatus::OffFileLeft);
    assert_eq!(harness.seen().len(), 2, "{:?}", harness.seen());
}
