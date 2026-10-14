//! The consent boundary through real manager/storage/daemon doubles.
use super::tests::{entry_state, turn, Daemon, Harness, FLATPAK};
use super::*;
use crate::curated::intent::IntentContext;
use crate::ws_messages::{
    CuratedIntentPurpose as Purpose, CuratedRequestOutcome as Outcome, ReplyTo,
};
use tokio::sync::mpsc;

const DNS: &str = "dns-resolved";
fn dns() -> &'static super::super::CuratedEntry {
    entries().iter().find(|entry| entry.id == DNS).unwrap()
}
pub(super) fn prepare(
    manager: &CuratedDefaults,
    context: &IntentContext,
    purpose: Purpose,
) -> String {
    let (tx, mut rx) = mpsc::channel(4);
    manager.try_route(ClientMessage::PrepareCuratedBroadOptIn {
        request_id: "intent-test".into(),
        id: DNS.into(),
        purpose,
        context: Some(context.clone()),
        reply: Some(ReplyTo::new(tx)),
    });
    let ServerMessage::CuratedBroadOptInPrepared { ticket, .. } = rx.try_recv().unwrap() else {
        panic!("no ticket")
    };
    ticket
}
pub(super) fn submit(
    manager: &CuratedDefaults,
    context: Option<IntentContext>,
    ticket: String,
    purpose: Purpose,
    reply: Option<ReplyTo>,
) {
    let message = if purpose == Purpose::Keep {
        ClientMessage::KeepCuratedBroadDefault {
            request_id: "intent-test".into(),
            id: DNS.into(),
            ticket,
            consent: store::BROAD_CONSENT.into(),
            purpose,
            context,
            reply,
        }
    } else {
        ClientMessage::EnableCuratedBroadDefault {
            request_id: "intent-test".into(),
            id: DNS.into(),
            ticket,
            consent: store::BROAD_CONSENT.into(),
            purpose,
            context,
            reply,
        }
    };
    assert!(manager.try_route(message).is_none());
}

#[tokio::test]
async fn legacy_singleton_and_mixed_are_private_ordered_partial_and_preserve_broad_choice() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let (tx, mut rx) = mpsc::channel(4);
    manager.try_route(ClientMessage::SetCuratedDefaults {
        ids: vec![DNS.into(), "unknown".into(), FLATPAK.into(), DNS.into()],
        on: true,
        request_id: Some("legacy".into()),
        reply: Some(ReplyTo::new(tx)),
    });
    assert!(!lock(&manager.inner.state).choices.enabled.contains(DNS));
    manager.reconcile().await;
    let ServerMessage::CuratedDefaultsRequestResult {
        request_id,
        results,
    } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(request_id.as_deref(), Some("legacy"));
    assert_eq!(
        results
            .iter()
            .map(|r| (r.id.as_str(), r.outcome))
            .collect::<Vec<_>>(),
        vec![
            (DNS, Outcome::RefusedConsent),
            ("unknown", Outcome::UnknownId),
            (FLATPAK, Outcome::ChoiceAccepted)
        ]
    );
    assert_eq!(harness.seen().len(), 1);
    assert!(entry_state(&manager, DNS)
        .problem
        .unwrap()
        .contains("Confirm"));
    let before = lock(&manager.inner.state).choices.clone();
    turn(&manager, DNS, true);
    assert_eq!(lock(&manager.inner.state).choices, before);
}

#[tokio::test]
async fn valid_modern_admission_persists_before_install_and_ticket_cannot_replay() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    let (tx, mut rx) = mpsc::channel(4);
    submit(
        &manager,
        Some(context.clone()),
        ticket.clone(),
        Purpose::Enable,
        Some(ReplyTo::new(tx)),
    );
    assert!(harness.seen().is_empty());
    manager.reconcile().await;
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(results[0].outcome, Outcome::ChoiceAccepted);
    assert_eq!(results[0].persisted, Some(true));
    assert!(store::load(&harness.file)
        .unwrap()
        .unwrap()
        .has_broad_authority(DNS));
    assert_eq!(entry_state(&manager, DNS).status, EntryStatus::Installed);
    let before = lock(&manager.inner.state).choices.clone();
    submit(&manager, Some(context), ticket, Purpose::Enable, None);
    assert_eq!(lock(&manager.inner.state).choices, before);
    manager.reconcile().await;
    assert_eq!(harness.seen().len(), 1);
}

#[tokio::test]
async fn unserverstamped_dead_cancelled_and_other_client_off_intents_refuse() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    submit(&manager, None, "a".repeat(64), Purpose::Enable, None);
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    turn(&manager, DNS, false);
    submit(
        &manager,
        Some(context.clone()),
        ticket,
        Purpose::Enable,
        None,
    );
    assert!(!lock(&manager.inner.state).choices.enabled.contains(DNS));
    let ticket = prepare(&manager, &context, Purpose::Enable);
    manager.try_route(ClientMessage::CancelCuratedBroadOptIn {
        request_id: "intent-test".into(),
        id: DNS.into(),
        context: Some(context.clone()),
        reply: None,
    });
    submit(
        &manager,
        Some(context.clone()),
        ticket,
        Purpose::Enable,
        None,
    );
    let ticket = prepare(&manager, &context, Purpose::Enable);
    drop(context.drop_guard());
    submit(&manager, Some(context), ticket, Purpose::Enable, None);
    manager.reconcile().await;
    assert!(harness.seen().is_empty());
}

#[tokio::test]
async fn disabled_keep_is_adoption_only_and_explicit_reenable_is_separate() {
    let mut disabled = dns().rule_as(Some(990));
    disabled.enabled = false;
    let harness = Harness::new().connect(Daemon::Accept, vec![disabled.clone()]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Keep);
    submit(&manager, Some(context.clone()), ticket, Purpose::Keep, None);
    manager.reconcile().await;
    assert!(harness.seen().is_empty());
    assert_eq!(
        entry_state(&manager, DNS).status,
        EntryStatus::InstalledButOff
    );
    assert!(!store::load(&harness.file)
        .unwrap()
        .unwrap()
        .has_broad_authority(DNS));
    assert_eq!(
        harness
            .commands
            .send_curated_toggle(&disabled.name, true)
            .err(),
        Some(crate::daemon_commands::SendError::ReservedName)
    );
    let ticket = prepare(&manager, &context, Purpose::Reenable);
    submit(&manager, Some(context), ticket, Purpose::Reenable, None);
    manager.reconcile().await;
    assert_eq!(harness.seen().len(), 1);
    assert_eq!(entry_state(&manager, DNS).status, EntryStatus::Installed);
}

#[tokio::test]
async fn rules_page_off_revokes_saved_authority_and_invalidates_pending_intent() {
    let harness = Harness::new().connect(Daemon::Accept, vec![dns().rule_as(Some(990))]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Keep);
    submit(&manager, Some(context.clone()), ticket, Purpose::Keep, None);
    manager.reconcile().await;
    assert!(store::load(&harness.file)
        .unwrap()
        .unwrap()
        .has_broad_authority(DNS));
    let ticket = prepare(&manager, &context, Purpose::Enable);
    manager
        .send_managed_toggle(&dns().rule_name(), false)
        .unwrap()
        .wait(COMMAND_TIMEOUT)
        .await
        .unwrap();
    submit(&manager, Some(context), ticket, Purpose::Enable, None);
    manager.reconcile().await;
    assert!(!store::load(&harness.file)
        .unwrap()
        .unwrap()
        .has_broad_authority(DNS));
    assert_eq!(
        entry_state(&manager, DNS).status,
        EntryStatus::InstalledButOff
    );
    assert_eq!(harness.seen().len(), 1);
}

#[tokio::test]
async fn failed_persistence_never_authorizes_install_and_reports_false() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    std::fs::create_dir(&harness.file).unwrap();
    let (tx, mut rx) = mpsc::channel(4);
    submit(
        &manager,
        Some(context),
        ticket,
        Purpose::Enable,
        Some(ReplyTo::new(tx)),
    );
    manager.reconcile().await;
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(results[0].choice_accepted, Some(true));
    assert_eq!(results[0].persisted, Some(false));
    assert!(manager.is_inert());
    assert!(harness.seen().is_empty());
}

#[tokio::test]
async fn stale_planned_install_refuses_even_after_a_new_modern_grant() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    submit(
        &manager,
        Some(context.clone()),
        ticket,
        Purpose::Enable,
        None,
    );
    manager.save_now();
    let old_revision = lock(&manager.inner.state).broad_revision;
    turn(&manager, DNS, false);
    let ticket = prepare(&manager, &context, Purpose::Enable);
    submit(&manager, Some(context), ticket, Purpose::Enable, None);
    manager.save_now();
    manager
        .apply(
            CuratedAction::Install(DNS.into()),
            manager.generation(),
            0,
            old_revision,
        )
        .await;
    assert!(harness.seen().is_empty());
    manager.reconcile().await;
    assert_eq!(harness.seen().len(), 1);
}

#[tokio::test]
async fn legacy_missing_dns_reports_awaiting_consent_using_old_wire_status() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let choices = store::Choices::default().enable(DNS);
    keep(&mut lock(&manager.inner.state), choices);
    manager.reconcile().await;
    let summary = entry_state(&manager, DNS);
    assert_eq!(summary.status, EntryStatus::NotInstalled);
    assert!(summary.on);
    assert!(summary.problem.unwrap().contains("awaitingConsent"));
    assert!(harness.seen().is_empty());
}

#[tokio::test]
async fn full_private_result_queue_refuses_before_any_new_choice() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    for _ in 0..256 {
        let (tx, _rx) = mpsc::channel(2);
        manager.try_route(ClientMessage::SetCuratedDefaults {
            ids: vec![FLATPAK.into()],
            on: true,
            request_id: Some("queued".into()),
            reply: Some(ReplyTo::new(tx)),
        });
    }
    let before = lock(&manager.inner.state).choices.clone();
    let (tx, mut rx) = mpsc::channel(4);
    manager.try_route(ClientMessage::SetCuratedDefaults {
        ids: vec![FLATPAK.into()],
        on: false,
        request_id: Some("overflow".into()),
        reply: Some(ReplyTo::new(tx.clone())),
    });
    assert_eq!(lock(&manager.inner.state).choices, before);
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(results[0].outcome, Outcome::Unavailable);
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    submit(
        &manager,
        Some(context),
        ticket,
        Purpose::Enable,
        Some(ReplyTo::new(tx)),
    );
    assert_eq!(lock(&manager.inner.state).choices, before);
    assert_eq!(lock(&manager.inner.state).request_results.len(), 256);
}

#[tokio::test]
async fn deletion_marker_requires_off_then_fresh_modern_on() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let choices = store::Choices::default().enable(DNS).user_removed(DNS);
    keep(&mut lock(&manager.inner.state), choices);
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    submit(
        &manager,
        Some(context.clone()),
        ticket,
        Purpose::Enable,
        None,
    );
    manager.reconcile().await;
    assert!(harness.seen().is_empty());
    assert!(lock(&manager.inner.state)
        .choices
        .deleted_by_user
        .contains(DNS));
    turn(&manager, DNS, false);
    let ticket = prepare(&manager, &context, Purpose::Enable);
    submit(&manager, Some(context), ticket, Purpose::Enable, None);
    manager.reconcile().await;
    assert_eq!(harness.seen().len(), 1);
    assert!(!lock(&manager.inner.state)
        .choices
        .deleted_by_user
        .contains(DNS));
}

#[tokio::test]
async fn accepted_choice_revoked_before_save_never_reports_saved_authority() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    let (tx, mut rx) = mpsc::channel(4);
    submit(
        &manager,
        Some(context),
        ticket,
        Purpose::Enable,
        Some(ReplyTo::new(tx)),
    );
    turn(&manager, DNS, false);
    manager.reconcile().await;
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(results[0].choice_accepted, Some(true));
    assert_eq!(results[0].persisted, Some(false));
    assert!(harness.seen().is_empty());
}

#[tokio::test]
async fn reenable_superseded_by_disabled_keep_does_not_claim_saved_activation() {
    let mut disabled = dns().rule_as(Some(990));
    disabled.enabled = false;
    let harness = Harness::new().connect(Daemon::Accept, vec![disabled]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let (tx, mut rx) = mpsc::channel(4);
    let ticket = prepare(&manager, &context, Purpose::Reenable);
    submit(
        &manager,
        Some(context.clone()),
        ticket,
        Purpose::Reenable,
        Some(ReplyTo::new(tx.clone())),
    );
    let ticket = prepare(&manager, &context, Purpose::Keep);
    submit(
        &manager,
        Some(context),
        ticket,
        Purpose::Keep,
        Some(ReplyTo::new(tx)),
    );
    manager.reconcile().await;
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(results[0].persisted, Some(false));
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(results[0].persisted, Some(true));
    assert!(harness.seen().is_empty());
}

#[tokio::test]
async fn failed_durable_off_blocks_session_and_never_claims_persisted_revocation() {
    use std::os::unix::fs::PermissionsExt;
    let harness = Harness::new().connect(Daemon::Accept, vec![dns().rule_as(Some(990))]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Keep);
    submit(&manager, Some(context), ticket, Purpose::Keep, None);
    manager.reconcile().await;
    let before = std::fs::read(&harness.file).unwrap();
    let parent = harness.file.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (tx, mut rx) = mpsc::channel(4);
    manager.try_route(ClientMessage::SetCuratedDefaults {
        ids: vec![DNS.into()],
        on: false,
        request_id: Some("off-save-failure".into()),
        reply: Some(ReplyTo::new(tx)),
    });
    manager.reconcile().await;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ServerMessage::CuratedDefaultsRequestResult { results, .. } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(results[0].persisted, Some(false));
    assert!(manager.is_inert());
    assert!(!lock(&manager.inner.state).choices.has_broad_authority(DNS));
    assert!(harness.seen().is_empty());
    assert_eq!(std::fs::read(&harness.file).unwrap(), before);
    // An unwritten revocation does not claim to survive restart.
    assert!(store::load(&harness.file)
        .unwrap()
        .unwrap()
        .has_broad_authority(DNS));
}

#[tokio::test]
async fn different_valid_request_id_consumes_ticket_without_mutation_or_install() {
    let harness = Harness::new().connect(Daemon::Accept, vec![]);
    let manager = harness.curated();
    let context = IntentContext::for_test();
    let ticket = prepare(&manager, &context, Purpose::Enable);
    let before = lock(&manager.inner.state).choices.clone();
    let (tx, mut rx) = mpsc::channel(4);
    manager.try_route(ClientMessage::EnableCuratedBroadDefault {
        request_id: "different-valid-id".into(),
        id: DNS.into(),
        ticket: ticket.clone(),
        consent: store::BROAD_CONSENT.into(),
        purpose: Purpose::Enable,
        context: Some(context.clone()),
        reply: Some(ReplyTo::new(tx)),
    });
    let ServerMessage::CuratedDefaultsRequestResult {
        request_id,
        results,
    } = rx.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(request_id.as_deref(), Some("different-valid-id"));
    assert_eq!(results[0].outcome, Outcome::StaleIntent);
    assert_eq!(lock(&manager.inner.state).choices, before);
    // The mismatch consumed the ticket, so its original ID cannot reuse it.
    submit(&manager, Some(context), ticket, Purpose::Enable, None);
    manager.reconcile().await;
    assert_eq!(lock(&manager.inner.state).choices, before);
    assert!(harness.seen().is_empty());
    assert!(!harness.file.exists());
}
