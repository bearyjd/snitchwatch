use super::*;

const POLICY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn prepare(context: &IntentContext) -> String {
    context
        .admit()
        .unwrap()
        .prepare(
            "req-1",
            "dns-resolved",
            CuratedIntentPurpose::Enable,
            POLICY,
            7,
        )
        .unwrap()
}

fn consume(context: &IntentContext, ticket: &str) -> Result<(), IntentError> {
    context.admit()?.consume(
        "req-1",
        ticket,
        "dns-resolved",
        CuratedIntentPurpose::Enable,
        POLICY,
        7,
        CONSENT_VERSION,
    )
}

#[test]
fn random_lower_hex_single_use_and_cross_connection_binding() {
    let first = IntentContext::for_test();
    let second = IntentContext::for_test();
    let ticket = prepare(&first);
    assert!(valid_ticket(&ticket));
    assert_eq!(consume(&second, &ticket), Err(IntentError::StaleIntent));
    assert_eq!(consume(&first, &ticket), Ok(()));
    assert_eq!(consume(&first, &ticket), Err(IntentError::StaleIntent));
    assert!(!format!("{first:?}").contains(&ticket));
}

#[test]
fn replacement_cancel_and_revision_invalidate() {
    let context = IntentContext::for_test();
    let old = prepare(&context);
    let new = prepare(&context);
    assert_ne!(old, new);
    assert_eq!(consume(&context, &old), Err(IntentError::StaleIntent));
    assert_eq!(consume(&context, &new), Err(IntentError::StaleIntent));
    let ticket = prepare(&context);
    assert!(!context.admit().unwrap().cancel("other", "dns-resolved"));
    assert!(context.admit().unwrap().cancel("req-1", "dns-resolved"));
    assert_eq!(consume(&context, &ticket), Err(IntentError::StaleIntent));
    let ticket = prepare(&context);
    assert_eq!(
        context.admit().unwrap().consume(
            "req-1",
            &ticket,
            "dns-resolved",
            CuratedIntentPurpose::Enable,
            POLICY,
            8,
            CONSENT_VERSION
        ),
        Err(IntentError::StaleIntent)
    );
    assert_eq!(consume(&context, &ticket), Err(IntentError::StaleIntent));
}

#[test]
fn purpose_policy_expiry_and_invalid_submission_consume() {
    let context = IntentContext::for_test();
    for (purpose, policy, consent) in [
        (CuratedIntentPurpose::Keep, POLICY, CONSENT_VERSION),
        (CuratedIntentPurpose::Enable, "different", CONSENT_VERSION),
        (CuratedIntentPurpose::Enable, POLICY, "wrong"),
    ] {
        let ticket = prepare(&context);
        assert!(context
            .admit()
            .unwrap()
            .consume(
                "req-1",
                &ticket,
                "dns-resolved",
                purpose,
                policy,
                7,
                consent
            )
            .is_err());
        assert_eq!(consume(&context, &ticket), Err(IntentError::StaleIntent));
    }
    let before = Instant::now();
    let ticket = prepare(&context);
    {
        let mut state = context.0.lock().unwrap();
        let pending = state.pending.as_mut().unwrap();
        assert!(pending.expires >= before + INTENT_LIFETIME);
        assert!(pending.expires <= Instant::now() + INTENT_LIFETIME);
        pending.expires = Instant::now();
    }
    assert_eq!(consume(&context, &ticket), Err(IntentError::StaleIntent));
}

#[test]
fn death_serializes_with_admission_and_queued_context_loses_authority() {
    let context = IntentContext::for_test();
    let ticket = prepare(&context);
    let mut admitted = context.admit().unwrap();
    let death = context.drop_guard();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        drop(death);
        done_tx.send(()).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(done_rx.try_recv().is_err());
    assert_eq!(
        admitted.consume(
            "req-1",
            &ticket,
            "dns-resolved",
            CuratedIntentPurpose::Enable,
            POLICY,
            7,
            CONSENT_VERSION
        ),
        Ok(())
    );
    drop(admitted);
    done_rx.recv().unwrap();
    thread.join().unwrap();
    assert!(matches!(context.admit(), Err(IntentError::StaleIntent)));
}

#[test]
fn concurrent_duplicate_submission_admits_once() {
    let context = IntentContext::for_test();
    let ticket = prepare(&context);
    let joins: Vec<_> = (0..2)
        .map(|_| {
            let context = context.clone();
            let ticket = ticket.clone();
            std::thread::spawn(move || consume(&context, &ticket))
        })
        .collect();
    let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(IntentError::StaleIntent))
            .count(),
        1
    );
}
