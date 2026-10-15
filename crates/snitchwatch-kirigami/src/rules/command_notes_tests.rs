use super::*;
use snitchwatch_bridge::rule_policy::RuleProblem;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;

fn result(id: &str, outcome: RuleCommandOutcome) -> ServerMessage {
    ServerMessage::RuleCommandResult {
        request_id: id.into(),
        outcome,
    }
}

fn note(text: &str) -> RuleCommandOutcome {
    RuleCommandOutcome::OkWithNote { note: text.into() }
}

fn listed(_: &str) -> RuleState {
    Some("on".into())
}

fn texts(notes: &CommandNotes) -> Vec<(&str, &str)> {
    notes
        .notes()
        .iter()
        .map(|n| (n.rule.as_str(), n.text.as_str()))
        .collect()
}

#[test]
fn each_awaited_result_leaves_its_note_once() {
    let mut notes = CommandNotes::default();
    notes.sent("rules-1".into(), "100-x", "x");
    assert_eq!(
        notes.on_message(&result("rules-1", note("may come back on")), listed),
        Some(true)
    );
    assert_eq!(texts(&notes), [("100-x", "may come back on")]);
    assert_eq!(notes.text(), "x: may come back on");
    assert_eq!(
        notes.on_message(&result("rules-1", note("again")), listed),
        None,
        "twice"
    );
}

/// PR #163 review: a command or result for one rule never clears another's.
#[test]
fn another_rules_command_and_ok_keep_a_note() {
    let mut notes = CommandNotes::default();
    notes.sent("x1".into(), "100-x", "x");
    notes.sent("y1".into(), "100-y", "y");
    notes.on_message(&result("x1", note("may come back on")), listed);
    assert_eq!(
        notes.on_message(&result("y1", RuleCommandOutcome::Ok), listed),
        Some(false)
    );
    assert!(!notes.sent("y2".into(), "100-y", "y"));
    notes.on_message(&result("y2", note("y's")), listed);
    assert_eq!(
        texts(&notes),
        [("100-x", "may come back on"), ("100-y", "y's")]
    );
    // The same rule's next command clears only its own note.
    assert!(notes.sent("x2".into(), "100-x", "x"));
    assert_eq!(texts(&notes), [("100-y", "y's")]);
}

#[test]
fn an_ok_clears_its_rules_note() {
    let mut notes = CommandNotes::default();
    notes.sent("x1".into(), "100-x", "x");
    notes.on_message(&result("x1", note("n")), listed);
    // A command sent before the note came, answered Ok after it.
    notes.sent("x2".into(), "100-x", "x");
    notes.sent("x3".into(), "100-x", "x");
    notes.on_message(&result("x2", note("n2")), listed);
    assert_eq!(
        notes.on_message(&result("x3", RuleCommandOutcome::Ok), listed),
        Some(true)
    );
    assert!(notes.notes().is_empty());
}

#[test]
fn a_result_nobody_here_asked_for_is_ignored() {
    let mut notes = CommandNotes::default();
    notes.sent("rules-1".into(), "100-x", "x");
    let editor = result("edit-1", note("n"));
    assert_eq!(notes.on_message(&editor, listed), None);
    let list = ServerMessage::SetRules { rules: vec![] };
    assert_eq!(notes.on_message(&list, listed), None);
    assert!(notes.notes().is_empty());
}

/// A note may contradict a rule that changed since it came.
#[test]
fn a_rule_that_changed_since_its_note_drops_it() {
    let mut notes = CommandNotes::default();
    notes.sent("x1".into(), "100-x", "x");
    notes.sent("y1".into(), "100-y", "y");
    notes.on_message(&result("x1", note("x's")), |_| Some("off".into()));
    notes.on_message(&result("y1", note("y's")), |_| None);
    assert!(!notes.rules_changed(|rule| match rule {
        "100-x" => Some("off".into()),
        _ => None,
    }));
    assert!(notes.rules_changed(|rule| match rule {
        "100-x" => Some("on".into()),
        _ => None,
    }));
    assert_eq!(texts(&notes), [("100-y", "y's")]);
    assert!(notes.rules_changed(|_| Some("back".into())));
    assert!(notes.notes().is_empty());
}

#[test]
fn a_new_session_drops_every_wait_and_note() {
    let mut notes = CommandNotes::default();
    assert!(!notes.note_session(1));
    notes.sent("x1".into(), "100-x", "x");
    notes.sent("x2".into(), "100-x", "x");
    notes.on_message(&result("x1", note("n")), listed);
    assert!(!notes.note_session(1));
    assert!(notes.note_session(2));
    assert!(notes.notes().is_empty());
    assert_eq!(notes.on_message(&result("x2", note("n")), listed), None);
}

#[test]
fn every_outcome_says_what_happened_in_plain_words() {
    let cases = [
        (
            RuleCommandOutcome::Rejected {
                reason: "bad".into(),
            },
            "Not changed: bad",
        ),
        (
            RuleCommandOutcome::Refused {
                problems: vec![RuleProblem {
                    path: "name".into(),
                    reason: "busy".into(),
                }],
            },
            "Not sent: ",
        ),
        (
            RuleCommandOutcome::Unsure {
                reason: "may or may not have applied".into(),
            },
            "may or may not have applied",
        ),
        (RuleCommandOutcome::Timeout, RULES_PAGE_WORDING.unknown),
        (RuleCommandOutcome::NoDaemon, RULES_PAGE_WORDING.no_daemon),
    ];
    for (outcome, starts) in cases {
        let mut notes = CommandNotes::default();
        notes.sent("r".into(), "100-x", "x");
        notes.on_message(&result("r", outcome.clone()), listed);
        let text = &notes.notes()[0].text;
        assert!(text.starts_with(starts), "{outcome:?}: {text}");
    }
}

#[test]
fn waits_and_notes_are_bounded() {
    let mut notes = CommandNotes::default();
    for n in 0..=MAX_WAITING {
        notes.sent(format!("r{n}"), &format!("{n}"), "");
    }
    assert_eq!(notes.on_message(&result("r0", note("n")), listed), None);
    for n in 1..=MAX_WAITING {
        notes.on_message(&result(&format!("r{n}"), note("n")), listed);
    }
    assert_eq!(notes.notes().len(), MAX_NOTES);
    assert_eq!(
        notes.notes()[MAX_NOTES - 1].rule,
        MAX_WAITING.to_string(),
        "the newest stays"
    );
}
