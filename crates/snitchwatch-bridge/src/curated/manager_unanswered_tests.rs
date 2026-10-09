//! Issue #120 items 13-17: an install with no answer may apply too, a
//! reconnect forgets what may apply, and a refused delete of a rule that
//! never had a file reads plain `Off`.
//!
//! The time is paused, so the 15 s command timeout passes at once, and each
//! pass is run directly (no pass gate), so the commands sent are exact.

use super::state::MaybeApplied;
use super::tests::*;
use super::*;
use snitchwatch_proto::protocol::{Action, Rule};

fn sent(action: Action) -> (i32, String) {
    (action as i32, FLATPAK_RULE.to_string())
}

fn status(curated: &CuratedDefaults) -> EntryStatus {
    entry_state(curated, FLATPAK).status
}

/// More passes, each after the same choice asked for again.
async fn forced_passes(curated: &CuratedDefaults, on: bool) {
    for _ in 0..3 {
        turn(curated, FLATPAK, on);
        curated.reconcile().await;
    }
}

/// Turned on, and the install gets no answer within the timeout: shown as
/// failed, and not sent again by more passes.
async fn unanswered_install(harness: &Harness, curated: &CuratedDefaults) -> u64 {
    turn(curated, FLATPAK, true);
    curated.reconcile().await;
    assert_eq!(status(curated), EntryStatus::NotInstalled);
    for _ in 0..2 {
        curated.reconcile().await;
    }
    assert_eq!(harness.seen(), vec![sent(Action::ChangeRule)]);
    harness.seen.lock().unwrap()[0].id
}

/// #13: the daemon may still take an unanswered install into memory, so
/// turning the entry off deletes it by name, once.
#[tokio::test(start_paused = true)]
async fn off_after_an_unanswered_install_deletes_it_once() {
    let harness = Harness::new().connect(Daemon::SilentInstalls, Vec::new());
    let curated = harness.curated();
    unanswered_install(&harness, &curated).await;

    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    let deleted = vec![sent(Action::ChangeRule), sent(Action::DeleteRule)];
    assert_eq!(harness.seen(), deleted);
    assert_eq!(status(&curated), EntryStatus::Off);
    forced_passes(&curated, false).await;
    assert_eq!(harness.seen(), deleted);
}

/// #13 (item 9): a late `ERROR` or `OK` within the grace period, then off:
/// one delete either way. A late `ERROR` leaves the list as it was; a late
/// `OK` lists the rule, and the listed copy is deleted.
#[tokio::test(start_paused = true)]
async fn off_after_a_late_answer_to_an_unanswered_install_deletes_it_once() {
    for (ok, answered) in [
        (false, EntryStatus::NotInstalled),
        (true, EntryStatus::Installed),
    ] {
        let harness = Harness::new().connect(Daemon::SilentInstalls, Vec::new());
        let curated = harness.curated();
        let install = unanswered_install(&harness, &curated).await;
        harness.answer(install, ok);
        curated.reconcile().await;
        assert_eq!(status(&curated), answered, "late OK: {ok}");

        turn(&curated, FLATPAK, false);
        curated.reconcile().await;
        let deleted = vec![sent(Action::ChangeRule), sent(Action::DeleteRule)];
        assert_eq!(harness.seen(), deleted, "late OK: {ok}");
        assert_eq!(status(&curated), EntryStatus::Off, "late OK: {ok}");
        forced_passes(&curated, false).await;
        assert_eq!(harness.seen(), deleted, "late OK: {ok}");
    }
}

/// #13: a closed stream is not a timeout: the reconnect's snapshot is the
/// daemon's memory, so nothing is recorded. Checked on the state itself, as
/// the reconnect would forget a record anyway (#14).
#[tokio::test(start_paused = true)]
async fn an_install_cut_off_by_a_closed_stream_records_nothing() {
    let mut harness = Harness::new().connect(Daemon::SilentInstalls, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    let pass = tokio::spawn({
        let curated = curated.clone();
        async move { curated.reconcile().await }
    });
    while harness.seen().is_empty() {
        tokio::task::yield_now().await;
    }
    drop(harness.stream.take());
    pass.await.unwrap();
    assert!(lock(&curated.inner.state).maybe_applied.is_empty());
}

/// #14: a reconnect's snapshot is the daemon's memory. A refused install
/// recorded on the old stream is forgotten: off sends nothing for a rule
/// the new list lacks.
#[tokio::test]
async fn a_reconnect_forgets_a_refused_install() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert_eq!(status(&curated), EntryStatus::NotInstalled);

    harness.resync(Vec::new());
    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    assert_eq!(harness.seen(), vec![sent(Action::ChangeRule)]);
    assert_eq!(status(&curated), EntryStatus::Off);
}

fn file_left(harness: &Harness) -> bool {
    harness
        .rules
        .cache()
        .lock()
        .unwrap()
        .files_left()
        .contains(FLATPAK_RULE)
}

/// Off, the delete refused, and the passes that follow it.
async fn off_refused(harness: &Harness, curated: &CuratedDefaults) {
    *harness.policy.lock().unwrap() = Daemon::Refuse;
    turn(curated, FLATPAK, false);
    curated.reconcile().await;
    for _ in 0..2 {
        curated.reconcile().await;
    }
}

/// #15: an unanswered install may have written its file, so the refused
/// delete after it keeps the "may come back" warning.
#[tokio::test(start_paused = true)]
async fn a_refused_delete_after_an_unanswered_install_keeps_the_file_warning() {
    let harness = Harness::new().connect(Daemon::SilentInstalls, Vec::new());
    let curated = harness.curated();
    unanswered_install(&harness, &curated).await;
    off_refused(&harness, &curated).await;
    assert_eq!(
        harness.seen(),
        vec![sent(Action::ChangeRule), sent(Action::DeleteRule)]
    );
    assert_eq!(status(&curated), EntryStatus::OffFileLeft);
    assert!(file_left(&harness));
}

/// #15: a refused install doesn't take back what an earlier unanswered one
/// may have written.
#[tokio::test(start_paused = true)]
async fn a_refused_install_after_an_unanswered_one_keeps_the_file_warning() {
    let harness = Harness::new().connect(Daemon::SilentInstalls, Vec::new());
    let curated = harness.curated();
    unanswered_install(&harness, &curated).await;
    *harness.policy.lock().unwrap() = Daemon::Refuse;
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert_eq!(status(&curated), EntryStatus::NotInstalled);
    off_refused(&harness, &curated).await;
    assert_eq!(
        harness.seen(),
        vec![
            sent(Action::ChangeRule),
            sent(Action::ChangeRule),
            sent(Action::DeleteRule)
        ]
    );
    assert_eq!(status(&curated), EntryStatus::OffFileLeft);
    assert!(file_left(&harness));
}

fn edited_flatpak() -> Rule {
    let mut rule = flatpak_rule();
    rule.operator.as_mut().unwrap().list[2].data = "8443".into();
    rule
}

/// #17: an edited copy's removal, once answered (`OK` or refused), took the
/// name out of the daemon's memory. A record of it that may apply (as from
/// a refused install on this stream) is dropped with it, so the passes
/// after it send no second delete.
#[tokio::test]
async fn an_answered_removal_forgets_what_may_apply() {
    for (daemon, after) in [
        (Daemon::Accept, EntryStatus::Off),
        (Daemon::RefuseDeletes, EntryStatus::OffFileLeft),
    ] {
        let harness = Harness::new().connect(daemon, vec![edited_flatpak()]);
        let curated = harness.curated();
        turn(&curated, FLATPAK, false);
        let record = MaybeApplied {
            generation: curated.generation(),
            file_possible: false,
        };
        lock(&curated.inner.state)
            .maybe_applied
            .insert(FLATPAK_RULE.into(), record);
        curated.try_route(ClientMessage::RemoveCuratedDefault { id: FLATPAK.into() });
        curated.reconcile().await;
        let removed = vec![sent(Action::DeleteRule)];
        assert_eq!(harness.seen(), removed);
        forced_passes(&curated, false).await;
        assert_eq!(harness.seen(), removed);
        assert_eq!(status(&curated), after);
    }
}
