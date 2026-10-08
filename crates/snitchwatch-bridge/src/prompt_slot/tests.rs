use super::*;

fn holder_of(message: &ServerMessage) -> (Option<String>, u32, Option<u64>) {
    match message {
        ServerMessage::PromptSlot {
            holder,
            holders,
            defaulted_at_least,
        } => (
            holder.as_ref().map(|h| h.row_id.clone()),
            *holders,
            *defaulted_at_least,
        ),
        other => panic!("expected PromptSlot, got {other:?}"),
    }
}

fn held(row_id: &str) -> PromptSlot {
    let mut slot = PromptSlot::default();
    slot.hold(row_id, "curl → example.com".to_string(), 1_000);
    slot
}

#[test]
fn a_hold_is_announced_with_its_holder_and_a_release_clears_it() {
    let mut slot = held("ask-1");
    assert_eq!(holder_of(&slot.message()), (Some("ask-1".into()), 1, None));
    assert_eq!(slot.release("ask-1"), Some(None));
    assert_eq!(holder_of(&slot.message()), (None, 0, None));
    assert_eq!(slot.release("ask-1"), None, "released once");
}

#[test]
fn the_oldest_holder_is_by_hold_order_not_by_row_id() {
    for (first, second) in [("ask-9", "ask-10"), ("ask-10", "ask-9")] {
        let mut slot = held(first);
        slot.hold(second, "x → y".to_string(), 1_000);
        assert_eq!(
            holder_of(&slot.message()),
            (Some(first.to_string()), 2, None),
            "{first} then {second}"
        );
    }
}

#[test]
fn the_count_is_unknown_until_a_reading_after_the_baseline() {
    let mut slot = held("ask-1");
    slot.observe(100, 500, 1);
    assert_eq!(holder_of(&slot.message()).2, None, "the baseline alone");
    slot.observe(107, 510, 1);
    assert_eq!(holder_of(&slot.message()).2, Some(7));
}

#[test]
fn a_reading_from_before_the_hold_is_never_the_baseline() {
    let mut slot = PromptSlot::default();
    // Misses counted before any prompt (say, before a GUI authenticated).
    slot.observe(50, 400, 1);
    slot.hold("ask-1", "x → y".to_string(), 1_000);
    slot.observe(100, 500, 1);
    slot.observe(103, 510, 1);
    assert_eq!(holder_of(&slot.message()).2, Some(3));
}

#[test]
fn a_daemon_restart_starts_a_fresh_baseline_without_underflow() {
    let mut slot = held("ask-1");
    slot.observe(100, 500, 1);
    slot.observe(110, 510, 1);
    assert_eq!(holder_of(&slot.message()).2, Some(10));
    // A new daemon process: uptime and rule_misses start over.
    slot.observe(3, 5, 1);
    assert_eq!(holder_of(&slot.message()).2, None);
    slot.observe(8, 15, 1);
    assert_eq!(holder_of(&slot.message()).2, Some(5));
}

#[test]
fn a_new_rule_snapshot_starts_a_fresh_baseline() {
    let mut slot = held("ask-1");
    slot.observe(100, 500, 1);
    slot.observe(110, 510, 1);
    // A new HELLO committed a snapshot: possibly another daemon connection.
    slot.observe(120, 520, 2);
    assert_eq!(holder_of(&slot.message()).2, None);
    slot.observe(124, 530, 2);
    assert_eq!(holder_of(&slot.message()).2, Some(4));
}

#[test]
fn releasing_one_holder_leaves_the_other_and_each_reports_its_own_count() {
    let mut slot = held("ask-1");
    slot.observe(100, 500, 1);
    slot.hold("ask-2", "x → y".to_string(), 2_000);
    slot.observe(104, 510, 1);
    slot.observe(110, 520, 1);
    assert_eq!(slot.release("ask-1"), Some(Some(10)));
    assert_eq!(
        holder_of(&slot.message()),
        (Some("ask-2".into()), 1, Some(6))
    );
    assert_eq!(slot.release("ask-2"), Some(Some(6)));
    assert_eq!(holder_of(&slot.message()), (None, 0, None));
}

#[test]
fn the_message_serializes_as_documented() {
    let slot = held("ask-3");
    assert_eq!(
        serde_json::to_value(slot.message()).unwrap(),
        serde_json::json!({
            "action": "promptSlot",
            "holder": { "rowId": "ask-3", "what": "curl → example.com", "sinceMs": 1000 },
            "holders": 1,
            "defaultedAtLeast": null,
        })
    );
    assert_eq!(
        serde_json::to_value(PromptSlot::default().message()).unwrap(),
        serde_json::json!({
            "action": "promptSlot", "holder": null, "holders": 0, "defaultedAtLeast": null,
        })
    );
}

#[test]
fn the_summary_strips_hazards_and_truncates_but_never_escapes() {
    assert_eq!(
        plain_summary("/tmp/<b>evil</b>\u{1b}[31m\u{202e}", "x&y.example"),
        "/tmp/<b>evil</b>[31m → x&y.example"
    );
    let long = "a".repeat(80);
    let summary = plain_summary(&long, "h");
    assert!(summary.starts_with(&"a".repeat(64)), "{summary}");
    assert!(summary.contains("…"), "{summary}");
}

fn handle() -> (
    PromptSlotHandle,
    broadcast::Receiver<ServerMessage>,
    broadcast::Receiver<Notice>,
) {
    let (tx, rx) = broadcast::channel(16);
    let notices = Arc::new(NoticeBus::new());
    let notice_rx = notices.subscribe();
    (PromptSlotHandle::new(tx, notices), rx, notice_rx)
}

#[test]
fn a_release_that_cost_connections_sends_one_summary_and_none_otherwise() {
    let (slot, _rx, mut notices) = handle();
    slot.hold("ask-1", "x → y".to_string());
    slot.observe(100, 500, 1);
    slot.observe(103, 510, 1);
    slot.release("ask-1", 1);
    assert_eq!(
        notices.try_recv().unwrap(),
        Notice::PromptSlotSummary {
            row_id: 1,
            count: 3
        }
    );
    assert!(notices.try_recv().is_err(), "one summary");

    // Zero, and unknown: no summary.
    slot.hold("ask-2", "x → y".to_string());
    slot.observe(103, 520, 1);
    slot.observe(103, 530, 1);
    slot.release("ask-2", 2);
    slot.hold("ask-3", "x → y".to_string());
    slot.release("ask-3", 3);
    assert!(notices.try_recv().is_err(), "no summary at 0 or unknown");
}

#[test]
fn observe_announces_only_a_change() {
    let (slot, mut rx, _notices) = handle();
    slot.hold("ask-1", "x → y".to_string());
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::PromptSlot { holders: 1, .. })
    ));
    slot.observe(100, 500, 1);
    assert!(
        rx.try_recv().is_err(),
        "the baseline changes nothing visible"
    );
    slot.observe(102, 510, 1);
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::PromptSlot {
            defaulted_at_least: Some(2),
            ..
        })
    ));
    slot.release("ask-1", 1);
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::PromptSlot { holders: 0, .. })
    ));
    slot.release("ask-1", 1);
    assert!(rx.try_recv().is_err(), "a second release announces nothing");
}

/// A snapshot interleaved with a release must not send the holder after the
/// release announced it gone (clients keep only the latest state). The
/// release runs while the snapshot is being sent, so it can only finish
/// after it.
#[test]
fn a_snapshot_is_sent_under_the_lock_so_a_racing_release_lands_after_it() {
    let (slot, mut rx, _notices) = handle();
    slot.hold("ask-1", "x → y".to_string());
    let _hold = rx.try_recv().unwrap();

    let mut releaser = None;
    let mut snapshot = None;
    slot.announce_with(|message| {
        let releasing = slot.clone();
        let thread = std::thread::spawn(move || releasing.release("ask-1", 1));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !thread.is_finished(),
            "a release completed while the snapshot was being sent"
        );
        releaser = Some(thread);
        snapshot = Some(message);
    });
    releaser.unwrap().join().unwrap();
    assert!(matches!(
        snapshot,
        Some(ServerMessage::PromptSlot { holders: 1, .. })
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::PromptSlot { holders: 0, .. })
    ));
}

#[test]
fn announce_sends_the_current_state() {
    let (slot, _rx, _notices) = handle();
    slot.hold("ask-1", "x → y".to_string());
    let (tx, mut snapshot_rx) = broadcast::channel(4);
    slot.announce(&tx);
    assert_eq!(snapshot_rx.try_recv().unwrap(), slot.message());
}
