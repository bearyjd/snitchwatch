//! Which rows are still waiting, and what a put-off row says (prompt-slot
//! plan Part C).
//!
//! A row is pending while it has no `action`, unless the bridge marked it
//! `deferred`: a prompt nobody answered in time, or one someone chose
//! "Decide later" for. Such a row can have no `action` either, when the
//! daemon applied its default action and the bridge doesn't know which. So
//! every "is it waiting?" check goes through [`is_pending`], and every
//! verdict through [`Verdict::of`]; otherwise a put-off row would look
//! waiting, be auto-selected and offer Allow.
//!
//! [`outcome_text`] is the verdict label of a deferred row. It is fixed text
//! and never names an action the bridge didn't report. "Usually" stays for
//! an allow: under nftables chain churn a requeued packet can be dropped
//! whatever the default action (bazzite-tower's r8 note).

use snitchwatch_bridge::ws_messages::ConnectionRow;

use super::row_store::Verdict;

/// Whether `row` still waits for an answer.
pub fn is_pending(row: &ConnectionRow) -> bool {
    row.action.is_none() && !row.deferred
}

impl Verdict {
    /// The verdict `row` shows. A deferred row with no known action is
    /// decided, by something this GUI can't name: [`Verdict::Other`].
    pub fn of(row: &ConnectionRow) -> Self {
        if row.action.is_none() && row.deferred {
            return Verdict::Other;
        }
        Verdict::from_action(row.action.as_deref())
    }
}

/// The verdict label of a deferred row; empty for every other row.
pub fn outcome_text(row: &ConnectionRow) -> &'static str {
    if !row.deferred {
        return "";
    }
    // A program blocked by "Decide later" has a rule on record; a default
    // action has none.
    if row.auto_answer.is_none() && row.matched_rule.is_some() {
        return "Decided later: blocked this program for 5 minutes on every host, even ones you allowed";
    }
    let by_nobody = row.auto_answer.is_some();
    match (by_nobody, row.action.as_deref()) {
        (true, Some("allow")) => {
            "Not answered in time: usually allowed (the firewall's default action)"
        }
        (true, Some("deny")) => "Not answered in time: denied (the firewall's default action)",
        (true, _) => "Not answered in time: the firewall's default action",
        (false, Some("allow")) => "Decided later: usually allowed (the firewall's default action)",
        (false, Some("deny")) => "Decided later: denied (the firewall's default action)",
        (false, _) => "Decided later: the firewall's default action",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::AutoAnswer;

    fn row(action: Option<&str>, deferred: bool) -> ConnectionRow {
        ConnectionRow {
            id: "1:ask-1".into(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: action.map(str::to_owned),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred,
        }
    }

    #[test]
    fn a_deferred_row_without_an_action_is_not_pending() {
        assert!(is_pending(&row(None, false)));
        assert!(!is_pending(&row(None, true)));
        assert!(!is_pending(&row(Some("allow"), false)));
        assert!(!is_pending(&row(Some("deny"), true)));
        assert_eq!(Verdict::of(&row(None, false)), Verdict::Pending);
        assert_eq!(Verdict::of(&row(None, true)), Verdict::Other);
        assert_eq!(Verdict::of(&row(Some("deny"), true)), Verdict::Denied);
        assert_eq!(Verdict::of(&row(Some("allow"), false)), Verdict::Allowed);
    }

    #[test]
    fn only_deferred_rows_have_an_outcome_text() {
        assert_eq!(outcome_text(&row(None, false)), "");
        assert_eq!(outcome_text(&row(Some("allow"), false)), "");
    }

    #[test]
    fn a_timed_out_row_names_the_action_only_when_the_bridge_knows_it() {
        let timed_out = |action| ConnectionRow {
            auto_answer: Some(AutoAnswer::NoAnswer),
            ..row(action, true)
        };
        assert_eq!(
            outcome_text(&timed_out(Some("allow"))),
            "Not answered in time: usually allowed (the firewall's default action)"
        );
        assert_eq!(
            outcome_text(&timed_out(Some("deny"))),
            "Not answered in time: denied (the firewall's default action)"
        );
        assert_eq!(
            outcome_text(&timed_out(None)),
            "Not answered in time: the firewall's default action"
        );
        // A reason from a newer bridge still reads as nobody answering.
        let newer = ConnectionRow {
            auto_answer: Some(AutoAnswer::Unknown),
            ..row(None, true)
        };
        assert_eq!(
            outcome_text(&newer),
            "Not answered in time: the firewall's default action"
        );
    }

    #[test]
    fn decide_later_says_blocked_only_with_a_rule_on_record() {
        let blocked = ConnectionRow {
            matched_rule: Some("deny-curl".into()),
            ..row(Some("deny"), true)
        };
        assert_eq!(
            outcome_text(&blocked),
            "Decided later: blocked this program for 5 minutes on every host, even ones you allowed"
        );
        assert_eq!(
            outcome_text(&row(Some("deny"), true)),
            "Decided later: denied (the firewall's default action)"
        );
        assert_eq!(
            outcome_text(&row(Some("allow"), true)),
            "Decided later: usually allowed (the firewall's default action)"
        );
        assert_eq!(
            outcome_text(&row(None, true)),
            "Decided later: the firewall's default action"
        );
    }
}
