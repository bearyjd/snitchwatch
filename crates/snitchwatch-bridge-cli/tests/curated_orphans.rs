//! #113 "No longer recommended" end to end: a system bridge and the mock
//! daemon, which keeps rules in memory and on disk the way opensnitchd does.
//! A rule under the curated prefix that no entry offers and Snitchwatch
//! recorded no copy of is listed; the Rules page still can't delete it, and
//! the Recommended page's Remove deletes exactly that rule, once.

use mock_opensnitchd::loader::LoaderModel;
use snitchwatch_bridge::curated::wire::CuratedOrphanSummary;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::BridgeMode;
use snitchwatch_proto::protocol::{Action, Operator, Rule};
use tokio::sync::broadcast;

mod curated_support;
use curated_support::{connect_model, next_command, nothing_sent, send, start, WAIT};

const RETIRED: &str = "retired-service";
const RETIRED_RULE: &str = "snitchwatch-default-retired-service";
const FLATPAK: &str = "flatpak-flathub";

fn retired_rule(enabled: bool) -> Rule {
    Rule {
        name: RETIRED_RULE.into(),
        enabled,
        action: "allow".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "process.path".into(),
            data: "/usr/libexec/retired-service".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Watch `SetCuratedDefaults` until its orphans satisfy `done`.
async fn orphans_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&[CuratedOrphanSummary]) -> bool,
) -> Vec<CuratedOrphanSummary> {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::SetCuratedDefaults { orphans, .. }) = rx.recv().await {
                if done(&orphans) {
                    return orphans;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"))
}

async fn snapshot_orphans(
    s: &mut curated_support::Setup,
    what: &str,
    done: impl Fn(&[CuratedOrphanSummary]) -> bool,
) -> Vec<CuratedOrphanSummary> {
    while s.rx.try_recv().is_ok() {}
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    orphans_until(&mut s.rx, what, done).await
}

#[tokio::test]
async fn an_orphan_is_listed_refused_by_the_rules_page_and_removed_from_recommended() {
    let mut s = start(BridgeMode::System).await;
    let model = LoaderModel::with_rules(&[retired_rule(false)]).shared();
    let (_daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    let listed = snapshot_orphans(&mut s, "the orphan listed", |o| !o.is_empty()).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, RETIRED);
    assert!(!listed[0].enabled && !listed[0].file_left && listed[0].problem.is_none());
    nothing_sent(&mut seen).await;

    // The Rules page's delete stays refused under the reserved prefix.
    send(
        &s.bridge,
        ClientMessage::DeleteRule {
            rule_id: RETIRED_RULE.into(),
            request_id: None,
            reply: None,
        },
    )
    .await;
    // Nor can a current entry, or a raw rule name, be removed this way.
    for id in [FLATPAK, RETIRED_RULE] {
        send(
            &s.bridge,
            ClientMessage::RemoveCuratedOrphan { id: id.into() },
        )
        .await;
    }
    nothing_sent(&mut seen).await;
    assert!(model.lock().unwrap().memory.contains_key(RETIRED_RULE));

    // Remove from the Recommended page: exactly one delete of that rule.
    send(
        &s.bridge,
        ClientMessage::RemoveCuratedOrphan { id: RETIRED.into() },
    )
    .await;
    let delete = next_command(&mut seen).await;
    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules.len(), 1);
    assert_eq!(delete.rules[0].name, RETIRED_RULE);
    orphans_until(&mut s.rx, "the orphan gone", |o| o.is_empty()).await;
    {
        let model = model.lock().unwrap();
        assert!(!model.memory.contains_key(RETIRED_RULE));
        assert!(!model.files.contains_key(RETIRED_RULE));
    }
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// Tower r12's immutable file: the daemon drops the rule from memory and
/// refuses. The page says the file may remain, offers no second Remove,
/// and nothing more is sent.
#[tokio::test]
async fn a_refused_orphan_removal_says_its_file_may_remain() {
    let mut s = start(BridgeMode::System).await;
    let model = LoaderModel::with_rules(&[retired_rule(true)]).shared();
    model.lock().unwrap().stuck.insert(RETIRED_RULE.into());
    let (_daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    snapshot_orphans(&mut s, "the orphan listed", |o| o.len() == 1).await;
    send(
        &s.bridge,
        ClientMessage::RemoveCuratedOrphan { id: RETIRED.into() },
    )
    .await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::DeleteRule as i32
    );
    let left = orphans_until(&mut s.rx, "file left", |o| o.len() == 1 && o[0].file_left).await;
    assert!(left[0].problem.is_some());
    nothing_sent(&mut seen).await;
    {
        let model = model.lock().unwrap();
        assert!(!model.memory.contains_key(RETIRED_RULE));
        assert!(model.files.contains_key(RETIRED_RULE));
    }
    s.bridge.shutdown();
}

/// The per-user bridge changes no recommended rules: it lists no orphans
/// and removes none.
#[tokio::test]
async fn the_per_user_bridge_lists_and_removes_no_orphans() {
    let mut s = start(BridgeMode::User).await;
    let model = LoaderModel::with_rules(&[retired_rule(true)]).shared();
    let (_daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    let none = snapshot_orphans(&mut s, "a snapshot", |_| true).await;
    assert!(none.is_empty(), "{none:?}");
    send(
        &s.bridge,
        ClientMessage::RemoveCuratedOrphan { id: RETIRED.into() },
    )
    .await;
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}
