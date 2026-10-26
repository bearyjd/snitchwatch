use super::*;

/// Issue #44: the fixed explanation, with no "Review" action.
#[test]
fn verdict_not_remembered_is_explained() {
    let notice = BridgeNotice::VerdictNotRemembered { row_id: 3 };
    let (summary, body) = notice_text(&notice).unwrap();
    assert_eq!(summary, "Snitchwatch — answer not remembered");
    assert_eq!(body, RuleRefusal::ProcessFileUnknown.describe());
}

/// Prompt-slot plan, part A: the release summary, fixed text around
/// the count, with no "Review" (the prompt is already answered).
#[test]
fn a_prompt_slot_summary_is_fixed_text() {
    let notice = BridgeNotice::PromptSlotSummary {
        row_id: 3,
        count: 2,
    };
    let (summary, body) = notice_text(&notice).unwrap();
    assert_eq!(summary, "Snitchwatch — while a prompt was open");
    assert_eq!(
        body,
        "While that prompt was open, the firewall applied its default action 2 times \
         (retries count again)."
    );
}

/// r11 issue A: a Pending notice held back because the window is
/// active doesn't take its cooldown, so it can go out once the window
/// isn't active, once. Other notices aren't held back by the window.
#[test]
fn a_notice_held_back_by_the_window_can_go_later() {
    let gate = Mutex::new(CooldownGate::new());
    let pending = BridgeNotice::Pending {
        row_id: 3,
        process: "curl".into(),
    };
    let t0 = Instant::now();
    let at = |secs| t0 + Duration::from_secs(secs);
    assert_eq!(
        may_post(&pending, None, true, &gate, at(0)),
        Gate::WindowActive
    );
    assert_eq!(
        may_post(&pending, None, true, &gate, at(1)),
        Gate::WindowActive
    );
    assert_eq!(may_post(&pending, None, false, &gate, at(2)), Gate::Post);
    assert_eq!(
        may_post(&pending, None, false, &gate, at(3)),
        Gate::CooledDown
    );
    assert_eq!(
        may_post(&BridgeNotice::DaemonAway, None, true, &gate, at(3)),
        Gate::Post
    );
}

/// #116 L5: a daemon retry is a new row for the same flow, so its
/// notice is held back; while the flow's first notice is live that is a
/// repeat (0d).
#[test]
fn a_retry_of_the_same_flow_is_held_back() {
    let gate = Mutex::new(CooldownGate::new());
    let pending = |row_id| BridgeNotice::Pending {
        row_id,
        process: "curl".into(),
    };
    let flow = FlowKey {
        session: 1,
        program: "/usr/bin/curl".into(),
        host: "example.com".into(),
    };
    let t0 = Instant::now();
    let at = |secs| t0 + Duration::from_secs(secs);
    assert_eq!(
        may_post(&pending(1), Some(&flow), false, &gate, at(0)),
        Gate::Post
    );
    assert_eq!(
        may_post(&pending(2), Some(&flow), false, &gate, at(5)),
        Gate::Repeat
    );
}

/// A prompt of this GUI's own session is held back while the alert is
/// the active window and posted when it is not, whatever the main
/// window does; other sessions follow the main window.
#[test]
fn the_alert_gates_this_guis_own_prompt_and_the_window_the_others() {
    assert!(front_is_active(true, false, true));
    assert!(!front_is_active(true, true, false));
    assert!(front_is_active(false, true, false));
    assert!(!front_is_active(false, false, true));
    let gate = Mutex::new(CooldownGate::new());
    let pending = BridgeNotice::Pending {
        row_id: 3,
        process: "curl".into(),
    };
    let now = Instant::now();
    let held = front_is_active(true, false, true);
    assert_eq!(
        may_post(&pending, None, held, &gate, now),
        Gate::WindowActive
    );
    let unseen = front_is_active(true, true, false);
    assert_eq!(may_post(&pending, None, unseen, &gate, now), Gate::Post);
}

/// 0d: a repeat of a flow's live notice is not posted again, a new flow
/// is, and a repeat held back by the window never started the hour.
#[test]
fn a_repeat_of_a_live_notice_is_coalesced() {
    let gate = Mutex::new(CooldownGate::new());
    let pending = |row_id| BridgeNotice::Pending {
        row_id,
        process: "curl".into(),
    };
    let flow = |host: &str| FlowKey {
        session: 1,
        program: "/usr/bin/curl".into(),
        host: host.into(),
    };
    let (curl, other) = (flow("example.com"), flow("example.org"));
    let now = Instant::now();
    let later = now + Duration::from_secs(40);
    // The window being active records nothing, so no hour has started.
    assert_eq!(
        may_post(&pending(1), Some(&curl), true, &gate, now),
        Gate::WindowActive
    );
    assert_eq!(
        may_post(&pending(1), Some(&curl), false, &gate, now),
        Gate::Post
    );
    assert_eq!(
        may_post(&pending(2), Some(&curl), false, &gate, later),
        Gate::Repeat
    );
    // Held back by the window, a repeat isn't counted either.
    assert_eq!(
        may_post(&pending(3), Some(&curl), true, &gate, later),
        Gate::WindowActive
    );
    assert_eq!(gate.lock().unwrap().suppressed(&curl), 1);
    assert_eq!(
        may_post(&pending(4), Some(&other), false, &gate, later),
        Gate::Post
    );
    // Within the 30 s cooldown and with no live notice: CooledDown.
    gate.lock().unwrap().clear_flow(&other);
    assert_eq!(
        may_post(&pending(5), Some(&other), false, &gate, later),
        Gate::CooledDown
    );
}

/// 0d: the live entry is cleared when the notice's task ends for any
/// reason, including one that never got as far as waiting.
#[test]
fn dropping_the_guard_clears_the_flow() {
    let gate = Arc::new(Mutex::new(CooldownGate::new()));
    let flow = FlowKey {
        session: 1,
        program: "/usr/bin/curl".into(),
        host: "example.com".into(),
    };
    let notice = BridgeNotice::Pending {
        row_id: 1,
        process: "curl".into(),
    };
    let now = Instant::now();
    assert_eq!(
        may_post(&notice, Some(&flow), false, &gate, now),
        Gate::Post
    );
    let guard = ClearFlowOnDrop::new(gate.clone(), HeldBackNotices::default(), flow.clone());
    let soon = now + Duration::from_secs(1);
    assert_eq!(
        may_post(&notice, Some(&flow), false, &gate, soon),
        Gate::Repeat
    );
    drop(guard);
    assert_eq!(gate.lock().unwrap().suppressed(&flow), 0);
    // Past the 30 s cooldown the next ask posts again.
    let later = now + Duration::from_secs(31);
    assert_eq!(
        may_post(&notice, Some(&flow), false, &gate, later),
        Gate::Post
    );
}

/// 0d: a poisoned gate still clears the flow.
#[test]
fn the_guard_clears_through_a_poisoned_gate() {
    let gate = Arc::new(Mutex::new(CooldownGate::new()));
    let flow = FlowKey {
        session: 1,
        program: "/usr/bin/curl".into(),
        host: "example.com".into(),
    };
    let notice = BridgeNotice::Pending {
        row_id: 1,
        process: "curl".into(),
    };
    let now = Instant::now();
    may_post(&notice, Some(&flow), false, &gate, now);
    let poisoner = gate.clone();
    let _ = std::thread::spawn(move || {
        let _held = poisoner.lock().unwrap();
        panic!("poison the gate");
    })
    .join();
    drop(ClearFlowOnDrop::new(
        gate.clone(),
        HeldBackNotices::default(),
        flow.clone(),
    ));
    let soon = now + Duration::from_secs(1);
    assert_eq!(
        may_post(&notice, Some(&flow), false, &gate, soon),
        Gate::CooledDown
    );
}

/// 0d: a guard that was told to hold leaves the flow coalesced; one that
/// was not (no outcome reported, or a non-Ended outcome) clears it.
#[test]
fn only_a_held_guard_keeps_the_flow_coalesced() {
    let flow = FlowKey {
        session: 1,
        program: "/usr/bin/curl".into(),
        host: "example.com".into(),
    };
    let notice = BridgeNotice::Pending {
        row_id: 1,
        process: "curl".into(),
    };
    let now = Instant::now();
    let soon = now + Duration::from_secs(40 * 60);
    for hold in [true, false] {
        let gate = Arc::new(Mutex::new(CooldownGate::new()));
        assert_eq!(
            may_post(&notice, Some(&flow), false, &gate, now),
            Gate::Post
        );
        let mut guard =
            ClearFlowOnDrop::new(gate.clone(), HeldBackNotices::default(), flow.clone());
        if hold {
            guard.hold();
        }
        drop(guard);
        let expected = if hold { Gate::Repeat } else { Gate::Post };
        assert_eq!(may_post(&notice, Some(&flow), false, &gate, soon), expected);
    }
}

/// 0d: a Repeat or a cooldown ends in `Done`, which leaves the row and the
/// banner alone; only the window asks to be looked at again.
#[test]
fn a_held_back_notice_reports_done() {
    assert_eq!(look_for(Gate::Repeat), PendingLook::Done);
    assert_eq!(look_for(Gate::CooledDown), PendingLook::Done);
    assert_eq!(look_for(Gate::Post), PendingLook::Done);
    assert_eq!(look_for(Gate::WindowActive), PendingLook::WindowActive);
}

/// 0d: only an Ended notice holds; Answered, Voided and NotShown let the
/// next ask notify.
#[test]
fn only_an_ended_notice_holds_the_repeat_window() {
    use crate::pending_notice::NoticeEnd;
    assert!(NoticeEnd::Ended.holds_the_repeat_window());
    for end in [NoticeEnd::Answered, NoticeEnd::Voided, NoticeEnd::NotShown] {
        assert!(!end.holds_the_repeat_window());
    }
}

/// The looks a prompt's notice gets, at what time after the prompt.
async fn looks(script: Vec<PendingLook>) -> Vec<Duration> {
    let start = tokio::time::Instant::now();
    let times = Arc::new(Mutex::new(Vec::new()));
    let seen = times.clone();
    let mut script = script.into_iter();
    post_once_the_window_is_not_active(1, 3, move || {
        seen.lock().unwrap().push(start.elapsed());
        let look = script.next().expect("looked again after the end");
        async move { look }
    })
    .await;
    let times = times.lock().unwrap().clone();
    times
}

/// r11 issue A: the prompt raised the window, so the window was active
/// when the grace period ended; it was closed later, with the prompt
/// still waiting. The notice goes out then, not never.
#[tokio::test(start_paused = true)]
async fn hiding_the_window_later_still_posts_the_notice() {
    use PendingLook::{Done, Gone, WindowActive};
    let secs = |list: &[u64]| -> Vec<Duration> {
        list.iter().map(|secs| Duration::from_secs(*secs)).collect()
    };
    // Posted at the end of the grace period, as before.
    assert_eq!(looks(vec![Done]).await, secs(&[5]));
    // Answered or silently handled before the first check: stop immediately.
    assert_eq!(looks(vec![Gone]).await, secs(&[5]));
    // Held back by the active window, then posted once it isn't.
    assert_eq!(
        looks(vec![WindowActive, WindowActive, WindowActive, Done]).await,
        secs(&[5, 6, 7, 8])
    );
    // Answered in the window: no more looks once the row stops waiting.
    assert_eq!(looks(vec![WindowActive, Gone]).await, secs(&[5, 6]));
}

/// PR #112 review L1: a notice is logged by kind and row only.
#[test]
fn a_notice_is_logged_without_its_connection_details() {
    let cases = [
        (
            BridgeNotice::Pending {
                row_id: 3,
                process: "/tmp/x/curl".into(),
            },
            ("pending", Some(3)),
        ),
        (
            BridgeNotice::DenyScopeNarrowed {
                row_id: 4,
                what: "/usr/bin/curl → example.com".into(),
                reason: "no program file".into(),
            },
            ("deny_scope_narrowed", Some(4)),
        ),
        (BridgeNotice::DaemonAway, ("daemon_away", None)),
    ];
    for (notice, label) in cases {
        assert_eq!(notice_label(&notice), label);
    }
}

/// A Pending notice never goes out as plain text: only through
/// `pending_notice`, with its actions heard from the server alone.
#[test]
fn a_pending_notice_has_no_plain_text_form() {
    let notice = BridgeNotice::Pending {
        row_id: 3,
        process: "curl".into(),
    };
    assert_eq!(notice_text(&notice), None);
}

/// 0d part 2: the held-back count follows the gate on every outcome, so it
/// never goes stale: a Post resets it, a Repeat sets it, the window being
/// active leaves it, a clear zeroes it and a cooldown leaves that 0.
#[test]
fn the_held_back_count_follows_every_outcome() {
    let gate = Mutex::new(CooldownGate::new());
    let notices = HeldBackNotices::default();
    let curl = FlowKey::of(1, Some("/usr/bin/curl"), "curl", "example.com");
    let pending = |row_id| BridgeNotice::Pending {
        row_id,
        process: "curl".into(),
    };
    let t0 = Instant::now();
    let at = |secs| t0 + Duration::from_secs(secs);
    let step = |row_id, window_active, now| {
        let verdict = may_post(&pending(row_id), Some(&curl), window_active, &gate, now);
        record_held_back(&gate, &curl, &notices, now);
        (verdict, notices.count(&curl, now))
    };
    assert_eq!(step(1, false, t0), (Gate::Post, 0));
    assert_eq!(step(2, false, at(40)), (Gate::Repeat, 1));
    assert_eq!(step(3, false, at(80)), (Gate::Repeat, 2));
    assert_eq!(step(4, true, at(90)), (Gate::WindowActive, 2));
    // Past the window, with no new ask, the count reads 0.
    assert_eq!(notices.count(&curl, at(3600)), 0);
    // A new hour starts from 0.
    assert_eq!(step(5, false, at(3600)), (Gate::Post, 0));
    assert_eq!(step(6, false, at(3610)), (Gate::Repeat, 1));
    // Cleared as the guard clears it: the gate, then the copy.
    let cleared = gate.lock().unwrap().clear_flow(&curl);
    notices.record(&curl, cleared, at(3615));
    assert_eq!(step(7, false, at(3620)), (Gate::CooledDown, 0));
}

/// 0d part 2: the guard that clears the flow clears its held-back count
/// too, unless it holds; the gate and the count are never locked together.
#[test]
fn the_guard_clears_the_held_back_count_unless_it_holds() {
    let curl = FlowKey::of(1, None, "curl", "example.com");
    let notice = BridgeNotice::Pending {
        row_id: 1,
        process: "curl".into(),
    };
    let t0 = Instant::now();
    let soon = t0 + Duration::from_secs(40);
    for hold in [true, false] {
        let gate = Arc::new(Mutex::new(CooldownGate::new()));
        let notices = HeldBackNotices::default();
        may_post(&notice, Some(&curl), false, &gate, t0);
        let mut guard = ClearFlowOnDrop::new(gate.clone(), notices.clone(), curl.clone());
        may_post(&notice, Some(&curl), false, &gate, soon);
        record_held_back(&gate, &curl, &notices, soon);
        assert_eq!(notices.count(&curl, soon), 1);
        if hold {
            guard.hold();
        }
        drop(guard);
        let expected = if hold { 1 } else { 0 };
        assert_eq!(notices.count(&curl, soon), expected, "hold: {hold}");
    }
}

/// Codex review of #230: `record_held_back` reads the gate, releases it,
/// then writes the count. A clear landing in between must win: the late
/// write carries the epoch it read, older than the clear's.
#[test]
fn a_clear_between_the_read_and_the_write_keeps_the_count_at_zero() {
    let gate = Arc::new(Mutex::new(CooldownGate::new()));
    let notices = HeldBackNotices::default();
    let curl = FlowKey::of(1, Some("/usr/bin/curl"), "curl", "example.com");
    let notice = BridgeNotice::Pending {
        row_id: 1,
        process: "curl".into(),
    };
    let t0 = Instant::now();
    let at = |secs| t0 + Duration::from_secs(secs);
    may_post(&notice, Some(&curl), false, &gate, t0);
    let guard = ClearFlowOnDrop::new(gate.clone(), notices.clone(), curl.clone());
    may_post(&notice, Some(&curl), false, &gate, at(5));
    record_held_back(&gate, &curl, &notices, at(5));
    assert_eq!(notices.count(&curl, at(5)), 1);
    may_post(&notice, Some(&curl), false, &gate, at(10));
    // Read under the gate lock, as `record_held_back` does...
    let read = gate.lock().unwrap().held_back(&curl);
    // ...then the notice ends (say, a new notification server)...
    drop(guard);
    // ...and only then the write.
    notices.record(&curl, read, at(10));
    assert_eq!(notices.count(&curl, at(10)), 0);
    // The flow's next posted notice counts again.
    assert_eq!(
        may_post(&notice, Some(&curl), false, &gate, at(40)),
        Gate::Post
    );
    may_post(&notice, Some(&curl), false, &gate, at(45));
    record_held_back(&gate, &curl, &notices, at(45));
    assert_eq!(notices.count(&curl, at(45)), 1);
}
