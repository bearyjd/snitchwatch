//! What the GUI says about the daemon's single prompt slot (plan
//! `docs/superpowers/plans/2026-10-08-prompt-slot-ux.md`, part A, plus the
//! honesty half of issue #78). Qt-free.
//!
//! **Wording.** The daemon's busy check sends other unmatched connections to
//! its default action while a prompt is open; tower's r8 VM saw requeued
//! packets dropped under nftables chain churn too, so the text says
//! "usually". The bridge doesn't know `DefaultAction`, so nothing here says
//! "allowed" or "blocked". The count is the daemon's `rule_misses`, which
//! counts unanswered packets, not connections: a retry, or the waiting
//! connection's own retransmit, counts again, so the text counts times.

/// Issue #78 for `holders` prompts holding the slot while filtering is
/// paused: the pause can't reach other new connections, which holds under
/// either default action. Fixed text, so the tray tooltip (rich text) may
/// show it too.
pub fn paused_while_waiting(holders: u32) -> String {
    if holders > 1 {
        format!(
            "Filtering is paused, but {holders} connections are still waiting for your answer. \
             Until you answer them, the pause can't reach other new connections; they usually \
             get the firewall's default action instead."
        )
    } else {
        "Filtering is paused, but a connection is still waiting for your answer. Until you \
         answer it, the pause can't reach other new connections; they usually get the \
         firewall's default action instead."
            .to_string()
    }
}

/// The slot as the banner describes it.
pub struct SlotView<'a> {
    /// "process → host" from the bridge: shown in a PlainText label only.
    pub what: &'a str,
    pub waited_secs: u64,
    pub holders: u32,
    /// The daemon's default-action count meanwhile (`None` while unknown).
    pub defaulted_at_least: Option<u64>,
    pub paused: bool,
}

/// The banner's text under its fixed heading.
pub fn banner_text(view: &SlotView<'_>) -> String {
    let mut text = format!(
        "{} has been waiting for your answer for {}.",
        view.what,
        waited(view.waited_secs)
    );
    if view.paused {
        text.push(' ');
        text.push_str(&paused_while_waiting(view.holders));
    } else {
        if view.holders > 1 {
            text.push_str(&format!(
                " {} prompts are open; this is the oldest.",
                view.holders
            ));
        }
        text.push_str(
            " Until you answer, other new connections usually get the firewall's default action.",
        );
    }
    if let Some(n) = view.defaulted_at_least.filter(|n| *n > 0) {
        let times = if n == 1 { "time" } else { "times" };
        text.push_str(&format!(
            " Meanwhile the firewall applied its default action {n} {times} (retries count \
             again)."
        ));
    }
    text
}

fn waited(secs: u64) -> String {
    if secs < 120 {
        format!("{secs} s")
    } else {
        format!("{} min {} s", secs / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAITED: &str = "steam → <b>cdn</b>.example has been waiting for your answer for 42 s.";
    const DEFAULTED: &str = " Until you answer, other new connections usually get the firewall's \
                             default action.";

    fn view(defaulted_at_least: Option<u64>, paused: bool) -> SlotView<'static> {
        SlotView {
            what: "steam → <b>cdn</b>.example",
            waited_secs: 42,
            holders: 1,
            defaulted_at_least,
            paused,
        }
    }

    /// The daemon's counter counts unanswered packets, not connections.
    #[test]
    fn the_banner_counts_times_the_default_action_was_applied() {
        assert_eq!(
            banner_text(&view(Some(3), false)),
            format!(
                "{WAITED}{DEFAULTED} Meanwhile the firewall applied its default action 3 times \
                 (retries count again)."
            )
        );
        assert!(banner_text(&view(Some(1), false))
            .ends_with(" applied its default action 1 time (retries count again)."));
    }

    #[test]
    fn an_unknown_or_zero_count_is_left_out() {
        for count in [None, Some(0)] {
            assert_eq!(
                banner_text(&view(count, false)),
                format!("{WAITED}{DEFAULTED}"),
                "{count:?}"
            );
        }
    }

    /// Issue #78: true under either default action, which the bridge
    /// doesn't know.
    #[test]
    fn a_pause_says_it_cant_reach_other_connections() {
        assert_eq!(
            paused_while_waiting(1),
            "Filtering is paused, but a connection is still waiting for your answer. Until you \
             answer it, the pause can't reach other new connections; they usually get the \
             firewall's default action instead."
        );
        assert_eq!(
            paused_while_waiting(2),
            "Filtering is paused, but 2 connections are still waiting for your answer. Until \
             you answer them, the pause can't reach other new connections; they usually get \
             the firewall's default action instead."
        );
        assert_eq!(
            banner_text(&view(Some(2), true)),
            format!(
                "{WAITED} {} Meanwhile the firewall applied its default action 2 times (retries \
                 count again).",
                paused_while_waiting(1)
            )
        );
        let mut several = view(None, true);
        several.holders = 3;
        assert_eq!(
            banner_text(&several),
            format!("{WAITED} {}", paused_while_waiting(3))
        );
    }

    #[test]
    fn several_open_prompts_are_counted() {
        let mut several = view(None, false);
        several.holders = 2;
        assert!(banner_text(&several).contains(" 2 prompts are open; this is the oldest."));
    }

    #[test]
    fn a_long_wait_reads_in_minutes() {
        let mut long = view(None, false);
        long.waited_secs = 150;
        assert!(banner_text(&long).contains("for 2 min 30 s."));
    }
}
