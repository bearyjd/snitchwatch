//! What the Rules page's list leaves out, in plain words (issue #61): the
//! bridge's `RulesNotShown`, sent after every rule list. Rules over the
//! per-rule size limits are left out one by one; a daemon list over
//! [`MAX_SNAPSHOT_RULES`] isn't read at all. Either way the rules stay in
//! the firewall service: a disabled one still doesn't apply, so the text
//! doesn't say they apply.

use snitchwatch_bridge::cache::rules::MAX_SNAPSHOT_RULES;
use snitchwatch_bridge::ws_messages::ServerMessage;

/// The page's text for a `RulesNotShown`; `None` for any other message,
/// and an empty string when nothing is left out.
pub fn not_shown_text(message: &ServerMessage) -> Option<String> {
    let ServerMessage::RulesNotShown {
        too_large,
        over_limit_total,
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
    Some(parts.join(" "))
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
        }
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
