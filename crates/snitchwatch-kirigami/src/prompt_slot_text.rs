//! What the GUI says about the daemon's single prompt slot (plan
//! `docs/superpowers/plans/2026-10-08-prompt-slot-ux.md`, part A, plus the
//! honesty half of issue #78). Qt-free.
//!
//! **Wording.** The daemon's busy check sends every other unmatched connection
//! to its default action while a prompt is open; tower's r8 VM saw requeued
//! packets dropped under nftables chain churn too, so the text says
//! "usually". The bridge doesn't know `DefaultAction`, so nothing here says
//! "allowed" or "blocked". While filtering is paused, the pause can't reach
//! those connections either: the sentence says the pause lets nothing else
//! through, which holds under either default.

/// Issue #78: filtering is paused while a prompt holds the slot. Fixed text,
/// so the tray tooltip (rich text) may show it too.
pub const PAUSED_WHILE_WAITING: &str = "Filtering is paused, but one connection is still \
     waiting for your answer. Until you answer it, the pause usually lets nothing else through.";

/// The slot as the banner describes it.
pub struct SlotView<'a> {
    /// "process → host" from the bridge: shown in a PlainText label only.
    pub what: &'a str,
    pub waited_secs: u64,
    pub holders: u32,
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
    if view.holders > 1 {
        text.push_str(&format!(
            " {} prompts are open; this is the oldest.",
            view.holders
        ));
    }
    let defaulted = view.defaulted_at_least.filter(|n| *n > 0);
    if view.paused {
        text.push(' ');
        text.push_str(PAUSED_WHILE_WAITING);
        if let Some(n) = defaulted {
            text.push_str(&format!(
                " At least {n} other connections got the firewall's default action so far."
            ));
        }
    } else {
        text.push_str(
            " Until you answer, other new connections usually get the firewall's default action",
        );
        if let Some(n) = defaulted {
            text.push_str(&format!(" (at least {n} so far)"));
        }
        text.push('.');
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

    fn view(defaulted_at_least: Option<u64>, paused: bool) -> SlotView<'static> {
        SlotView {
            what: "steam → <b>cdn</b>.example",
            waited_secs: 42,
            holders: 1,
            defaulted_at_least,
            paused,
        }
    }

    #[test]
    fn the_banner_names_the_holder_and_what_waiting_costs() {
        assert_eq!(
            banner_text(&view(Some(3), false)),
            "steam → <b>cdn</b>.example has been waiting for your answer for 42 s. Until you \
             answer, other new connections usually get the firewall's default action (at least \
             3 so far)."
        );
    }

    #[test]
    fn an_unknown_or_zero_count_is_left_out() {
        for count in [None, Some(0)] {
            assert_eq!(
                banner_text(&view(count, false)),
                "steam → <b>cdn</b>.example has been waiting for your answer for 42 s. Until \
                 you answer, other new connections usually get the firewall's default action.",
                "{count:?}"
            );
        }
    }

    #[test]
    fn a_pause_says_it_lets_nothing_else_through() {
        let text = banner_text(&view(Some(2), true));
        assert!(text.contains(PAUSED_WHILE_WAITING), "{text}");
        assert!(
            text.ends_with(
                "At least 2 other connections got the firewall's default action so far."
            ),
            "{text}"
        );
        assert!(!text.contains("Until you answer, other"), "{text}");
        assert!(!banner_text(&view(None, true)).contains("At least"));
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
