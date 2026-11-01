//! Restyle batch E (rounded, 44 px): per-file source guards for the connection
//! list, the learning review, the curated-groups step and the onboarding page.
//! Exact counts of what is restyled and of what is deliberately left plain,
//! palette-only colours, and the row clamp / highlight tokens. The geometry
//! itself is measured in `rounded_batch_e_qml.rs`.
mod qml_guard_support;
use qml_guard_support::code_lines;

const ROW: &str = include_str!("../qml/ConnectionRow.qml");
const LIST: &str = include_str!("../qml/ConnectionList.qml");
const REVIEW_ROW: &str = include_str!("../qml/LearningReviewRow.qml");
const REVIEW_PAGE: &str = include_str!("../qml/LearningReviewPage.qml");
const STEP: &str = include_str!("../qml/CuratedGroupsStep.qml");
const ONBOARDING: &str = include_str!("../qml/OnboardingPage.qml");

/// (file, source, plain `Controls.Button`, `RoundedButton`, plain
/// `Controls.ToolButton`, `GuardedToolButton`, plain `Controls.CheckBox`).
/// `RoundedCheckBox` (restyle batch C) replaces the one tick box.
const COUNTS: &[(&str, &str, usize, usize, usize, usize, usize)] = &[
    ("ConnectionRow.qml", ROW, 0, 4, 0, 1, 0),
    ("ConnectionList.qml", LIST, 0, 0, 0, 0, 0),
    ("LearningReviewRow.qml", REVIEW_ROW, 0, 1, 0, 0, 0),
    ("LearningReviewPage.qml", REVIEW_PAGE, 0, 6, 0, 0, 0),
    ("CuratedGroupsStep.qml", STEP, 0, 2, 0, 0, 0),
    ("OnboardingPage.qml", ONBOARDING, 0, 2, 0, 0, 0),
];

#[test]
fn remaining_plain_controls_are_exactly_the_documented_ones() {
    for (name, src, button, rounded, tool, guarded_tool, check) in COUNTS {
        let code = code_lines(src);
        assert_eq!(
            code.matches("Controls.Button {").count(),
            *button,
            "{name}: plain Controls.Button"
        );
        assert_eq!(
            code.matches("RoundedButton {").count(),
            *rounded,
            "{name}: RoundedButton"
        );
        assert_eq!(
            code.matches("Controls.ToolButton {").count(),
            *tool,
            "{name}: plain ToolButton"
        );
        assert_eq!(
            code.matches("GuardedToolButton {").count(),
            *guarded_tool,
            "{name}: GuardedToolButton"
        );
        assert_eq!(
            code.matches("Controls.CheckBox {").count(),
            *check,
            "{name}: plain CheckBox"
        );
        let tick = usize::from(*name == "LearningReviewRow.qml");
        assert_eq!(
            code.matches("RoundedCheckBox {").count(),
            tick,
            "{name}: RoundedCheckBox"
        );
        assert_eq!(
            code.matches("Controls.ComboBox {").count(),
            0,
            "{name}: ComboBox"
        );
        assert_eq!(
            code.matches("Controls.Switch {").count(),
            0,
            "{name}: plain Switch"
        );
        // A plain button is never swapped for the held-key wrapper.
        let guarded = code.matches("GuardedButton {").count();
        let expect_guarded =
            usize::from(*name == "LearningReviewPage.qml" || *name == "OnboardingPage.qml");
        assert_eq!(guarded, expect_guarded, "{name}: GuardedButton count");
    }
}

#[test]
fn colours_are_palette_only() {
    for (name, src, ..) in COUNTS {
        let code = code_lines(src);
        for line in code.lines() {
            assert!(!line.contains("\"#"), "{name}: hex colour literal: {line}");
            assert!(!line.contains("Qt.rgba("), "{name}: rgba literal: {line}");
        }
    }
}

#[test]
fn the_connection_row_is_at_least_hit_min_high() {
    let code = code_lines(ROW);
    assert!(
        code.contains("implicitHeight: Math.max(Theme.hitMin,"),
        "ConnectionRow lost its Theme.hitMin row-height clamp"
    );
    assert!(
        ROW.lines().count() < 400,
        "ConnectionRow.qml is over its 400 line budget"
    );
}

#[test]
fn the_list_draws_one_rounded_inset_selection_highlight() {
    let code = code_lines(LIST);
    assert_eq!(
        code.matches("highlight:").count(),
        1,
        "ConnectionList: one highlight"
    );
    assert!(code.contains("objectName: \"selectionHighlight\""));
    assert!(code.contains("radius: Theme.radiusMedium"));
    assert!(
        code.contains("anchors.margins: Theme.spacingSmall"),
        "highlight not inset"
    );
    assert!(code.contains("highlightMoveDuration: 0"));
    assert!(code.contains("border.width: list.activeFocus ? Theme.focusRingWidth : 0"));
}

#[test]
fn the_review_row_card_takes_its_radius_from_theme() {
    let code = code_lines(REVIEW_ROW);
    assert!(code.contains("radius: Theme.radiusMedium"));
    assert!(!code.contains("radius: 3"));
    assert!(code.contains("import com.snitchwatch.shell"));
}

#[test]
fn the_guarded_tool_button_draws_its_own_label() {
    // KDE's native style paints a button's text inside its background; a
    // replaced background without a contentItem renders blank on Plasma.
    let code = code_lines(include_str!("../qml/GuardedToolButton.qml"));
    assert!(
        code.contains("contentItem: Impl.IconLabel {"),
        "GuardedToolButton lost its own label"
    );
}
