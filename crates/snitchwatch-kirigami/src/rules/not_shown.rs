//! What the Rules page's list leaves out, in plain words (issue #61): the
//! bridge's `RulesNotShown`, sent after every rule list. Rules over the
//! per-rule size limits are left out one by one; a daemon list over
//! [`MAX_SNAPSHOT_RULES`] isn't read at all. Either way the rules stay in
//! the firewall service: a disabled one still doesn't apply, so the text
//! doesn't say they apply. Deleted rules whose saved files the service
//! couldn't remove are no longer listed, but may come back (tower r12).

use snitchwatch_bridge::cache::rules::MAX_SNAPSHOT_RULES;
use snitchwatch_bridge::ws_messages::ServerMessage;

/// The page's text for a `RulesNotShown`; `None` for any other message,
/// and an empty string when nothing is left out.
pub fn not_shown_text(message: &ServerMessage) -> Option<String> {
    let ServerMessage::RulesNotShown {
        too_large,
        over_limit_total,
        left_on_disk,
        ..
    } = message
    else {
        return None;
    };
    let mut parts = Vec::new();
    if let Some(total) = over_limit_total {
        parts.push(format!(
            "The firewall service has {} rules, more than the {} Snitchwatch reads, so none \
             are listed. They are still in the firewall service.",
            grouped(u64::from(*total)),
            grouped(MAX_SNAPSHOT_RULES as u64),
        ));
    }
    match too_large {
        0 => {}
        1 => parts.push(
            "1 rule isn't listed: it is larger than Snitchwatch reads. It is still in the \
             firewall service."
                .into(),
        ),
        n => parts.push(format!(
            "{} rules aren't listed: each is larger than Snitchwatch reads. They are still in \
             the firewall service.",
            grouped(u64::from(*n)),
        )),
    }
    match left_on_disk {
        0 => {}
        1 => parts.push(
            "1 deleted rule may come back when the firewall service restarts: the service \
             stopped using it, but couldn't remove its saved file."
                .into(),
        ),
        n => parts.push(format!(
            "{} deleted rules may come back when the firewall service restarts: the service \
             stopped using them, but couldn't remove their saved files.",
            grouped(u64::from(*n)),
        )),
    }
    Some(parts.join(" "))
}

/// The Rules page's hint when the firewall service keeps reporting a
/// different number of rules than the list holds (issue #65). One fixed
/// text: nothing the service says is rendered. The count can't say why, and
/// an edit of a rule file in place leaves it alone, so the sentence is a
/// possibility and a remedy, never a claim about the cause.
pub const COUNT_MISMATCH_HINT: &str = "The firewall service reports a different number of rules \
     than this list shows, so rule files may have been changed outside Snitchwatch. Restarting \
     the firewall service refreshes the list.";

/// The page's hint for a `RulesNotShown`: [`COUNT_MISMATCH_HINT`] while the
/// bridge says the counts differ and it has a list to differ from, and an
/// empty string otherwise; `None` for any other message.
pub fn count_hint_text(message: &ServerMessage) -> Option<&'static str> {
    let ServerMessage::RulesNotShown {
        listed,
        count_mismatch,
        ..
    } = message
    else {
        return None;
    };
    Some(if *listed && *count_mismatch {
        COUNT_MISMATCH_HINT
    } else {
        ""
    })
}

/// `n` with thousands separators (12,000).
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(too_large: u32, over_limit_total: Option<u32>) -> ServerMessage {
        ServerMessage::RulesNotShown {
            too_large,
            over_limit_total,
            listed: over_limit_total.is_none(),
            left_on_disk: 0,
            count_mismatch: false,
        }
    }

    #[test]
    fn deleted_rules_that_may_come_back_are_said_plainly() {
        let left = |left_on_disk| ServerMessage::RulesNotShown {
            too_large: 0,
            over_limit_total: None,
            listed: true,
            left_on_disk,
            count_mismatch: false,
        };
        let one = not_shown_text(&left(1)).unwrap();
        assert!(
            one.starts_with("1 deleted rule may come back") && one.contains("its saved file"),
            "{one}"
        );
        let many = not_shown_text(&left(1_200)).unwrap();
        assert!(
            many.starts_with("1,200 deleted rules may come back") && many.contains("their"),
            "{many}"
        );
        let both = not_shown_text(&ServerMessage::RulesNotShown {
            too_large: 2,
            over_limit_total: None,
            listed: true,
            left_on_disk: 1,
            count_mismatch: false,
        })
        .unwrap();
        assert!(
            both.starts_with("2 rules aren't listed") && both.ends_with("saved file."),
            "{both}"
        );
    }

    fn mismatch(listed: bool, count_mismatch: bool) -> ServerMessage {
        ServerMessage::RulesNotShown {
            too_large: 0,
            over_limit_total: None,
            listed,
            left_on_disk: 0,
            count_mismatch,
        }
    }

    #[test]
    fn the_count_hint_is_one_fixed_sentence_pair_shown_only_for_a_listed_mismatch() {
        assert_eq!(
            count_hint_text(&mismatch(true, true)),
            Some(COUNT_MISMATCH_HINT)
        );
        assert_eq!(count_hint_text(&mismatch(true, false)), Some(""));
        // With no list from the firewall service there is nothing to differ from.
        assert_eq!(count_hint_text(&mismatch(false, true)), Some(""));
        assert_eq!(count_hint_text(&ServerMessage::ClearConnectionRows), None);
    }

    #[test]
    fn the_count_hint_is_honest_actionable_and_plain() {
        let hint = COUNT_MISMATCH_HINT;
        assert!(hint.contains("different number of rules"), "{hint}");
        assert!(hint.contains("changed outside Snitchwatch"), "{hint}");
        assert!(
            hint.contains("may have been"),
            "it is a possibility: {hint}"
        );
        assert!(
            hint.contains("Restarting the firewall service refreshes the list"),
            "{hint}"
        );
        assert!(!hint.contains(['<', '>', '&', '\n']), "plain text: {hint}");
        // The mismatch says nothing else is left out.
        assert_eq!(not_shown_text(&mismatch(true, true)).as_deref(), Some(""));
    }

    #[test]
    fn what_is_left_out_is_said_plainly() {
        assert_eq!(not_shown_text(&message(0, None)).as_deref(), Some(""));
        let one = not_shown_text(&message(1, None)).unwrap();
        assert!(one.starts_with("1 rule isn't listed"), "{one}");
        let many = not_shown_text(&message(3, None)).unwrap();
        assert!(many.starts_with("3 rules aren't listed"), "{many}");
        assert!(!many.contains("apply"), "a disabled rule doesn't: {many}");
        let over = not_shown_text(&message(0, Some(12_000))).unwrap();
        assert!(
            over.contains("12,000 rules") && over.contains("the 10,000 Snitchwatch reads"),
            "{over}"
        );
        assert!(over.contains("none are listed"), "{over}");
        let many = not_shown_text(&message(1_234_567, None)).unwrap();
        assert!(many.starts_with("1,234,567 rules aren't listed"), "{many}");
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(not_shown_text(&ServerMessage::ClearConnectionRows), None);
    }
}
