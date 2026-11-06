//! Selectable text, batch 2 (Onboarding and silent mode; plan
//! `2026-10-30-selectable-text.md`). Source guards: every class-A text of the
//! batch's files is a `SelectableText`, and the few texts that stay a
//! `Controls.Label` / `Kirigami.Heading` are exactly the listed, justified
//! ones. A page that grows a new plain Label, or loses a conversion, fails.
//!
//! Mouse only: converted texts must not add a Tab stop, so no
//! `activeFocusOnTab` / `focus` override may appear in these files either.
//! Layout unchanged: a TextEdit wraps by default and cannot elide, so every
//! converted block's `wrapMode` is pinned per block in `WRAPS` (changing any
//! block's mode fails) and none sets `elide`. `activeFocusOnPress` (false in
//! `SelectableText.qml`) may not be overridden: a click must not steal focus.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block, in any order).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    // The level-2 state title / heading are SelectableText { level: 2 }.
    (
        "OnboardingPage.qml",
        include_str!("../qml/OnboardingPage.qml"),
        4,
        &[],
    ),
    (
        "SilentModeChoice.qml",
        include_str!("../qml/SilentModeChoice.qml"),
        3,
        &[],
    ),
    // Left: "I understand ..." has a TapHandler that ticks the box; a
    // selecting TextEdit would eat that click.
    (
        "LearningStartPanel.qml",
        include_str!("../qml/LearningStartPanel.qml"),
        4,
        &["onTapped: understoodBox.toggle()"],
    ),
    (
        "LearningBanner.qml",
        include_str!("../qml/LearningBanner.qml"),
        5,
        &[],
    ),
    (
        "PromptSlotBanner.qml",
        include_str!("../qml/PromptSlotBanner.qml"),
        1,
        &[],
    ),
    (
        "BackgroundGroupBar.qml",
        include_str!("../qml/BackgroundGroupBar.qml"),
        4,
        &[],
    ),
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

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "OnboardingPage.qml",
        &[
            ("Daemon not installed", "Text.WordWrap"),
            ("wizardDetail", "Text.WordWrap"),
            ("See docs/packaging/rpm-ostree-layering.md", "Text.WordWrap"),
            ("Retrying automatically", "Text.WordWrap"),
        ],
    ),
    (
        "SilentModeChoice.qml",
        &[
            ("When a program wants the network", "Text.WordWrap"),
            ("modelData.detail", "Text.Wrap"),
            ("notRunningNote", "Text.Wrap"),
        ],
    ),
    (
        "LearningStartPanel.qml",
        &[
            ("Learning records which programs", "Text.Wrap"),
            ("text: \"For\"", "Text.NoWrap"),
            ("warningLabel", "Text.Wrap"),
            ("errorLabel", "Text.Wrap"),
        ],
    ),
    (
        "LearningBanner.qml",
        &[
            ("summaryLabel", "Text.Wrap"),
            ("incompleteLabel", "Text.Wrap"),
            ("droppedLabel", "Text.Wrap"),
            ("noticeLabel", "Text.Wrap"),
            ("whereLabel", "Text.WrapAnywhere"),
        ],
    ),
    ("PromptSlotBanner.qml", &[("holderLabel", "Text.Wrap")]),
    (
        "BackgroundGroupBar.qml",
        &[
            ("showingText", "Text.WordWrap"),
            ("backgroundSummary", "Text.WordWrap"),
            ("newProgramsText", "Text.WordWrap"),
            ("baselineText", "Text.WordWrap"),
        ],
    ),
    ("GroupPendingBadge.qml", &[]),
    ("GroupVerdictCounts.qml", &[]),
];

/// (file, [(marker unique to one converted Heading block, its exact level)]).
const LEVELS: &[(&str, &[(&str, u32)])] = &[
    ("OnboardingPage.qml", &[("Daemon not installed", 2)]),
    (
        "SilentModeChoice.qml",
        &[("When a program wants the network", 2)],
    ),
];

#[test]
fn batch_2_headings_keep_their_level() {
    for (name, source, ..) in FILES {
        let converted = blocks(&code_lines(source), "SelectableText {");
        let expected = LEVELS
            .iter()
            .find(|(n, _)| n == name)
            .map_or(&[][..], |(_, l)| *l);
        let leveled = converted
            .iter()
            .filter(|b| b.lines().any(|l| l.trim().starts_with("level:")))
            .count();
        assert_eq!(
            leveled,
            expected.len(),
            "{name}: SelectableText blocks with a `level:`"
        );
        for (marker, level) in expected {
            let hits: Vec<&String> = converted.iter().filter(|b| b.contains(marker)).collect();
            assert_eq!(
                hits.len(),
                1,
                "{name}: marker `{marker}` must match one block"
            );
            let want = format!("level: {level}");
            assert!(
                hits[0].lines().any(|l| l.trim() == want),
                "{name}: heading `{marker}` must have `{want}`:\n{}",
                hits[0]
            );
        }
    }
}

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
            for forbidden in [
                "activeFocusOnTab",
                "focus:",
                "KeyNavigation",
                "forceActiveFocus",
                "activeFocusOnPress",
            ] {
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
        let code = code_lines(source);
        let converted = blocks(&code, "SelectableText {");
        for block in &converted {
            assert!(
                !block.contains("elide:"),
                "{name}: a SelectableText sets `elide` (a TextEdit cannot elide):\n{block}"
            );
        }
        let (_, expected) = WRAPS
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name}: missing from WRAPS"));
        assert_eq!(
            converted.len(),
            expected.len(),
            "{name}: WRAPS entries vs blocks"
        );
        for (marker, wrap) in *expected {
            let hits: Vec<&String> = converted.iter().filter(|b| b.contains(marker)).collect();
            assert_eq!(
                hits.len(),
                1,
                "{name}: marker `{marker}` must match one block"
            );
            let want = format!("wrapMode: {wrap}");
            assert!(
                hits[0].lines().any(|l| l.trim() == want),
                "{name}: block `{marker}` must have `{want}`:\n{}",
                hits[0]
            );
        }
    }
}
