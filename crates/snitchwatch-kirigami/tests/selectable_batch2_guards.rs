//! Selectable text, batch 2 (Onboarding and silent mode; plan
//! `2026-10-30-selectable-text.md`). Source guards: every class-A text of the
//! batch's files is a `SelectableText`, and the few texts that stay a
//! `Controls.Label` / `Kirigami.Heading` are exactly the listed, justified
//! ones. A page that grows a new plain Label, or loses a conversion, fails.
//!
//! Mouse only: converted texts must not add a Tab stop, so no
//! `activeFocusOnTab` / `focus` override may appear in these files either.
//! Layout unchanged: a TextEdit wraps by default and cannot elide, so every
//! converted block names its `wrapMode` and none sets `elide`.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block, in any order).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    // Left: the level-2 Heading (SelectableText has no `level` until batch 1).
    (
        "OnboardingPage.qml",
        include_str!("../qml/OnboardingPage.qml"),
        3,
        &["Daemon not installed"],
    ),
    (
        "SilentModeChoice.qml",
        include_str!("../qml/SilentModeChoice.qml"),
        2,
        &["When a program wants the network"],
    ),
    // Left: "I understand ..." has a TapHandler that ticks the box; a
    // selecting TextEdit would eat that click.
    (
        "LearningStartPanel.qml",
        include_str!("../qml/LearningStartPanel.qml"),
        4,
        &["onTapped: understoodBox.toggle()"],
    ),
    ("LearningBanner.qml", include_str!("../qml/LearningBanner.qml"), 5, &[]),
    ("PromptSlotBanner.qml", include_str!("../qml/PromptSlotBanner.qml"), 1, &[]),
    ("BackgroundGroupBar.qml", include_str!("../qml/BackgroundGroupBar.qml"), 4, &[]),
    // Left: both sit on a ConnectionRow group header (a clickable row, batch
    // 8); a selecting TextEdit there would eat the header's click. The verdict
    // count is also elided.
    (
        "GroupPendingBadge.qml",
        include_str!("../qml/GroupPendingBadge.qml"),
        0,
        &["pendingBadgeLabel"],
    ),
    (
        "GroupVerdictCounts.qml",
        include_str!("../qml/GroupVerdictCounts.qml"),
        0,
        &["verdictWindowLabel", "verdictCount"],
    ),
];

#[test]
fn batch_2_texts_are_selectable() {
    for (name, source, selectable, kept) in FILES {
        let code = code_lines(source);
        let converted = blocks(&code, "SelectableText {").len();
        assert_eq!(converted, *selectable, "{name}: SelectableText blocks");
        let mut left = blocks(&code, "Controls.Label {");
        left.extend(blocks(&code, "Kirigami.Heading {"));
        assert_eq!(
            left.len(),
            kept.len(),
            "{name}: plain Label/Heading blocks left:\n{}",
            left.join("\n---\n")
        );
        for snippet in *kept {
            assert!(
                left.iter().any(|b| b.contains(snippet)),
                "{name}: expected a kept Label containing `{snippet}`"
            );
        }
    }
}

#[test]
fn batch_2_adds_no_tab_stop() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            for forbidden in ["activeFocusOnTab", "focus:", "KeyNavigation", "forceActiveFocus"] {
                assert!(
                    !block.contains(forbidden),
                    "{name}: a SelectableText sets `{forbidden}` (mouse only):\n{block}"
                );
            }
        }
    }
}

#[test]
fn batch_2_keeps_layout_equivalent() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            assert!(
                block.contains("wrapMode:"),
                "{name}: a SelectableText names no wrapMode (TextEdit wraps by default):\n{block}"
            );
            assert!(
                !block.contains("elide:"),
                "{name}: a SelectableText sets `elide` (a TextEdit cannot elide):\n{block}"
            );
        }
    }
}
