use std::cell::Cell;
use std::time::Duration;

use snitchwatch_bridge::ws_messages::RuleCommandOutcome as O;

use super::*;
use crate::bulk_rules::tests::fake::{Answer, Fake};
use crate::bulk_rules::{BatchState, RuleSink, Settings};

fn plan_of_two() -> BulkPlan {
    let s = session(vec![narrow("alpha")]);
    let key = key_of(&s, "/usr/bin/alpha");
    s.tick(&key, Some(K::AllowHosts))
        .unwrap()
        .freeze()
        .unwrap()
        .authorize()
        .unwrap()
        .clone()
}

fn run_with(fake: &mut Fake, learning: bool) -> (BatchOutcome, usize) {
    let deletes = Cell::new(0);
    let outcome = run_batch(
        &plan_of_two(),
        fake,
        &Settings::default(),
        &|| learning,
        &mut || {
            deletes.set(deletes.get() + 1);
            Ok(())
        },
        &mut |_| {},
    );
    (outcome, deletes.get())
}

#[test]
fn clear_is_refused_while_learning_or_applying_and_only_then() {
    assert!(clear_refusal(true, false).is_some());
    assert!(clear_refusal(false, true).is_some());
    assert!(clear_refusal(true, true).is_some());
    assert!(clear_refusal(false, false).is_none());
}

#[test]
fn the_record_is_deleted_after_a_completed_apply_when_learning_is_not_running() {
    let mut fake = Fake::new();
    let (outcome, deletes) = run_with(&mut fake, false);
    assert_eq!(outcome.progress.state, BatchState::Finished);
    assert!(outcome.deleted);
    assert_eq!(deletes, 1);
}

#[test]
fn the_record_is_kept_while_learning_runs() {
    let mut fake = Fake::new();
    let (outcome, deletes) = run_with(&mut fake, true);
    assert_eq!(outcome.progress.state, BatchState::Finished);
    assert!(!outcome.deleted);
    assert_eq!(deletes, 0);
}

#[test]
fn the_record_is_kept_when_a_rule_was_refused_or_the_batch_stopped() {
    let mut refused = Fake::with_answers(&[Answer::Not(O::Rejected {
        reason: "no".into(),
    })]);
    let (outcome, deletes) = run_with(&mut refused, false);
    assert!(!outcome.deleted && deletes == 0, "{:?}", outcome.progress);

    let mut cancelled = Fake::new();
    cancelled.cancel_after_adds = Some(1);
    let (outcome, deletes) = run_with(&mut cancelled, false);
    assert_ne!(outcome.progress.state, BatchState::Finished);
    assert!(!outcome.deleted && deletes == 0);

    let mut no_list = Fake::new();
    no_list.listed = false;
    let (outcome, deletes) = run_with(&mut no_list, false);
    assert!(matches!(outcome.progress.state, BatchState::Aborted(_)));
    assert!(!outcome.deleted && deletes == 0);
}

#[test]
fn the_rules_list_is_read_through_the_sink() {
    let mut fake = Fake::new();
    fake.rules = vec![crate::bulk_rules::tests::fake::payload_with("r1", "/usr/bin/x").to_wire()];
    let rules = read_rules(&mut fake, Duration::from_secs(5)).expect("a list");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].name, "r1");
}

#[test]
fn an_unlisted_or_silent_bridge_gives_no_rules_list() {
    let mut withdrawn = Fake::new();
    withdrawn.listed = false;
    assert!(read_rules(&mut withdrawn, Duration::from_secs(2)).is_none());
    let mut silent = Fake::new();
    silent.answers_snapshot = false;
    assert!(read_rules(&mut silent, Duration::from_secs(2)).is_none());
}

#[test]
fn a_deletion_that_failed_is_reported_and_not_called_done() {
    let mut fake = Fake::new();
    let outcome = run_batch(
        &plan_of_two(),
        &mut fake,
        &Settings::default(),
        &|| false,
        &mut || Err("unlink failed".into()),
        &mut |_| {},
    );
    assert!(!outcome.deleted);
    assert_eq!(outcome.delete_error.as_deref(), Some("unlink failed"));
}

/// A bridge whose reads panic.
struct Panics(Fake);

impl RuleSink for Panics {
    fn new_request_id(&mut self) -> String {
        self.0.new_request_id()
    }
    fn send_add(
        &mut self,
        id: &str,
        rule: &crate::learning_review::RulePayload,
    ) -> Result<(), crate::bulk_rules::SendError> {
        self.0.send_add(id, rule)
    }
    fn request_snapshot(&mut self) -> Result<(), crate::bulk_rules::SendError> {
        self.0.request_snapshot()
    }
    fn next_message(
        &mut self,
        _wait: Duration,
    ) -> Result<crate::bulk_rules::Incoming, crate::bulk_rules::SessionLost> {
        panic!("sink failed");
    }
    fn sleep(&mut self, d: Duration) {
        self.0.sleep(d)
    }
    fn now(&self) -> Duration {
        self.0.now()
    }
    fn cancelled(&self) -> bool {
        false
    }
}

#[test]
fn a_panic_in_the_batch_comes_back_as_an_error_not_an_unwind() {
    let mut sink = Panics(Fake::new());
    let deleted = Cell::new(false);
    let result = run_batch_guarded(
        &plan_of_two(),
        &mut sink,
        &Settings::default(),
        &|| false,
        &mut || {
            deleted.set(true);
            Ok(())
        },
        &mut |_| {},
    );
    assert!(result.unwrap_err().contains("stopped unexpectedly"));
    assert!(!deleted.get(), "nothing was deleted after a panic");
}

#[test]
fn delete_record_re_checks_learning_after_taking_the_gate() {
    use crate::settings_learning::SettingsStore;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};

    let store = Arc::new(SettingsStore::new());
    let learning = Arc::new(AtomicBool::new(false));
    let cleared = Arc::new(AtomicBool::new(false));

    // The Qt thread is applying a snapshot (holding the gate) ...
    let applying = store.gate();
    let (about_to_delete, started) = mpsc::channel();
    let worker = {
        let (store, learning, cleared) = (store.clone(), learning.clone(), cleared.clone());
        std::thread::spawn(move || {
            about_to_delete.send(()).unwrap();
            delete_record(
                &store,
                &|| learning.load(Ordering::SeqCst),
                &|| false,
                &mut || {
                    cleared.store(true, Ordering::SeqCst);
                    Ok(())
                },
            )
        })
    };
    started.recv().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // ... and that snapshot starts a window; only then does the gate open.
    learning.store(true, Ordering::SeqCst);
    drop(applying);
    let result = worker.join().unwrap();
    assert!(result.is_err(), "a window that began in the gap was wiped");
    assert!(!cleared.load(Ordering::SeqCst));
}

#[test]
fn delete_record_clears_when_idle_and_refuses_while_applying() {
    use crate::settings_learning::SettingsStore;
    let store = SettingsStore::new();
    let cleared = Cell::new(0);
    let mut clear = || {
        cleared.set(cleared.get() + 1);
        Ok(())
    };
    assert!(delete_record(&store, &|| false, &|| false, &mut clear).is_ok());
    assert!(delete_record(&store, &|| false, &|| true, &mut clear).is_err());
    assert!(delete_record(&store, &|| true, &|| false, &mut clear).is_err());
    assert_eq!(cleared.get(), 1);
}
