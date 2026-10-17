//! #113 "No longer recommended": the worker lists orphans and deletes one
//! only when asked, and only if it is still an orphan when sent.

use super::super::tests::{entry_state, Daemon, Harness, FLATPAK};
use super::*;
use crate::curated::wire::CuratedOrphanSummary;
use snitchwatch_proto::protocol::{Action, Rule};

const RETIRED: &str = "retired-service";
const RETIRED_RULE: &str = "snitchwatch-default-retired-service";
const OTHER: &str = "other-retired";
const OTHER_RULE: &str = "snitchwatch-default-other-retired";

fn orphan_rule(name: &str, enabled: bool) -> Rule {
    Rule {
        name: name.into(),
        enabled,
        action: "allow".into(),
        duration: "always".into(),
        ..Default::default()
    }
}

fn orphans_of(curated: &CuratedDefaults) -> Vec<CuratedOrphanSummary> {
    match curated.message() {
        ServerMessage::SetCuratedDefaults { orphans, .. } => orphans,
        other => panic!("{other:?}"),
    }
}

fn listed(id: &str, enabled: bool) -> CuratedOrphanSummary {
    CuratedOrphanSummary {
        id: id.into(),
        enabled,
        file_left: false,
        problem: None,
    }
}

fn ask(curated: &CuratedDefaults, id: &str) {
    curated.try_route(ClientMessage::RemoveCuratedOrphan { id: id.into() });
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

#[tokio::test]
async fn an_orphan_is_listed_and_removed_only_when_asked() {
    let harness = Harness::new().connect(
        Daemon::Accept,
        vec![
            orphan_rule(RETIRED_RULE, true),
            orphan_rule(OTHER_RULE, false),
        ],
    );
    let curated = harness.curated();
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    // A disabled orphan is listed too, as disabled.
    assert_eq!(
        orphans_of(&curated),
        vec![listed(OTHER, false), listed(RETIRED, true)]
    );

    ask(&curated, RETIRED);
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::DeleteRule as i32, RETIRED_RULE.to_string())]
    );
    assert_eq!(orphans_of(&curated), vec![listed(OTHER, false)]);
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "sent again: {:?}", harness.seen());
}

/// Nothing but a listed orphan is removed this way: not a current entry's
/// rule (even one the daemon lists), an invalid id, or a name not listed.
#[tokio::test]
async fn a_removal_of_anything_but_an_orphan_sends_nothing() {
    let entry_rule = super::super::tests::flatpak_rule();
    let harness = Harness::new().connect(
        Daemon::Accept,
        vec![entry_rule, orphan_rule(RETIRED_RULE, true)],
    );
    let curated = harness.curated();
    curated.reconcile().await;
    for id in [FLATPAK, "", "../x", RETIRED_RULE, "Bad_Id", "not-listed"] {
        ask(&curated, id);
    }
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::InFirewall
    );
}

/// A no-longer-offered copy Snitchwatch recorded installing is the plan's
/// (deleted only unedited), never an orphan: neither listed nor removed.
#[tokio::test]
async fn a_recorded_copy_is_not_an_orphan() {
    // Snitchwatch's copy of an entry since retired, edited since.
    let mut recorded = super::super::tests::flatpak_rule();
    recorded.name = RETIRED_RULE.into();
    let mut edited = recorded.clone();
    edited.operator.as_mut().unwrap().list[2].data = "8443".into();
    let harness = Harness::new().connect(Daemon::Accept, vec![edited]);
    store::save(
        &harness.file,
        &store::Choices::default().installed(RETIRED, &recorded),
    )
    .unwrap();
    let curated = harness.curated();
    curated.reconcile().await;
    assert!(orphans_of(&curated).is_empty());
    ask(&curated, RETIRED);
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
}

/// The send-time re-check: an orphan that left the list while an earlier
/// removal of the pass waited for the daemon is not sent.
#[tokio::test]
async fn an_orphan_that_is_gone_by_send_time_is_not_sent() {
    let harness = Harness::new().connect(
        Daemon::Accept,
        vec![
            orphan_rule(OTHER_RULE, true),
            orphan_rule(RETIRED_RULE, true),
        ],
    );
    let curated = harness.curated();
    curated.reconcile().await;
    ask(&curated, OTHER);
    ask(&curated, RETIRED);
    let gate = Arc::new(tokio::sync::Notify::new());
    *harness.hold.lock().unwrap() = Some(gate.clone());
    let pass = tokio::spawn({
        let curated = curated.clone();
        async move { curated.reconcile().await }
    });
    eventually("the first delete", || !harness.seen().is_empty()).await;
    // Same daemon stream: the second one leaves the list meanwhile.
    harness.rules.cache().lock().unwrap().remove(RETIRED_RULE);
    gate.notify_one();
    pass.await.unwrap();
    assert_eq!(
        harness.seen(),
        [(Action::DeleteRule as i32, OTHER_RULE.to_string())]
    );
}

/// The daemon refused: it dropped the rule from memory but its file may
/// remain. Listed with that reason and not removable, until the daemon
/// reconnects; listed again, it can be removed again.
#[tokio::test]
async fn a_refused_removal_says_its_file_is_left_until_a_reconnect() {
    let harness =
        Harness::new().connect(Daemon::RefuseDeletes, vec![orphan_rule(RETIRED_RULE, true)]);
    let curated = harness.curated();
    curated.reconcile().await;
    ask(&curated, RETIRED);
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1);
    let refused = orphans_of(&curated);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].file_left && !refused[0].enabled);
    assert_eq!(refused[0].problem.as_deref(), Some(ORPHAN_FILE_LEFT_REASON));
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "sent again: {:?}", harness.seen());

    // The daemon restarts and loads the file again.
    harness.resync(vec![orphan_rule(RETIRED_RULE, true)]);
    curated.reconcile().await;
    assert_eq!(orphans_of(&curated), vec![listed(RETIRED, true)]);
}

/// Any other failure: the orphan stays listed with the reason, on the same
/// daemon stream, and isn't sent again by itself.
#[tokio::test(start_paused = true)]
async fn an_unanswered_removal_is_shown_as_failed() {
    let harness =
        Harness::new().connect(Daemon::SilentDeletes, vec![orphan_rule(RETIRED_RULE, true)]);
    let curated = harness.curated();
    curated.reconcile().await;
    ask(&curated, RETIRED);
    curated.reconcile().await;
    let failed = orphans_of(&curated);
    assert_eq!(failed.len(), 1);
    assert!(!failed[0].file_left);
    assert_eq!(
        failed[0].problem.as_deref(),
        Some("The firewall service didn't answer.")
    );
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "sent again: {:?}", harness.seen());
}

/// An inert bridge lists none and removes none.
#[tokio::test]
async fn an_inert_bridge_lists_and_removes_no_orphans() {
    let harness = Harness::new().connect(Daemon::Accept, vec![orphan_rule(RETIRED_RULE, true)]);
    let curated = harness.curated();
    curated.set_unavailable("per-user");
    curated.reconcile().await;
    assert!(orphans_of(&curated).is_empty());
    ask(&curated, RETIRED);
    curated.reconcile().await;
    assert!(harness.seen().is_empty(), "{:?}", harness.seen());
}
