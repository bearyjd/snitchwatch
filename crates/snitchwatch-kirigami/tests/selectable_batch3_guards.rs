//! Selectable text, batch 3 (Learning review; plan
//! `2026-10-30-selectable-text.md`). Source guards: every class-A text of the
//! batch's files is a `SelectableText`; the texts that stay a
//! `Controls.Label` / `Kirigami.Heading` are exactly the listed, justified
//! ones; every class-C row carries one `RowCopyMenu` with a non-empty copy
//! text. A file that grows a new plain Label, or loses a conversion, fails.
//!
//! Mouse only: nothing here may add a Tab stop, so no `activeFocusOnTab` /
//! `focus` override may appear in these files.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks, RowCopyMenu blocks, snippets that
/// identify each text still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, usize, &[&str])] = &[
    ("LearningReviewPage.qml", include_str!("../qml/LearningReviewPage.qml"), 10, 0, &[]),
    // Class C: every Label in the row stays (a drag-select would fight the
    // list's flick, and a second right-click menu would open beside the row's).
    // The tick title also carries the row's TapHandler.
    (
        "LearningReviewRow.qml",
        include_str!("../qml/LearningReviewRow.qml"),
        0,
        1,
        &[
            "row.info.programPathFull",
            "row.countsText",
            "font.bold: true\n                text: modelData\n",
            "row.info.dualUse === true",
            "row.info.highFanOut === true",
            "modelData.host",
            "id: notSelectable",
            "row.info.alreadyPresent",
            "modelData.title",
            "modelData.hosts.join",
            "modelData.note",
        ],
    ),
    ("LearningProgressList.qml", include_str!("../qml/LearningProgressList.qml"), 3, 0, &[]),
    ("LearningLimitsList.qml", include_str!("../qml/LearningLimitsList.qml"), 1, 0, &[]),
    // Class C: the group rows. Title, description and the entry texts stay
    // Labels (the entry's program is elided: ElideMiddle). Heading: no `level`.
    (
        "CuratedGroupsStep.qml",
        include_str!("../qml/CuratedGroupsStep.qml"),
        3,
        1,
        &[
            "Recommended rules",
            "groupRow.modelData.title",
            "groupDescription-",
            "entryProgram",
            "entryAllows",
            "modelData.why",
            "modelData.statusText",
        ],
    ),
];

#[test]
fn batch_3_texts_are_selectable_or_justified() {
    for (name, source, selectable, _, kept) in FILES {
        let code = code_lines(source);
        assert_eq!(
            blocks(&code, "SelectableText {").len(),
            *selectable,
            "{name}: SelectableText blocks"
        );
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
fn batch_3_rows_have_a_copy_menu_with_text() {
    for (name, source, _, menus, _) in FILES {
        let found = blocks(&code_lines(source), "RowCopyMenu {");
        assert_eq!(found.len(), *menus, "{name}: RowCopyMenu blocks");
        for block in found {
            let has_copy_text = block
                .lines()
                .any(|l| l.trim().starts_with("copyText:") && l.trim().len() > "copyText:".len());
            assert!(has_copy_text, "{name}: a RowCopyMenu without copyText:\n{block}");
        }
    }
}

#[test]
fn batch_3_adds_no_tab_stop() {
    for (name, source, ..) in FILES {
        let code = code_lines(source);
        for open in ["SelectableText {", "RowCopyMenu {"] {
            for block in blocks(&code, open) {
                for forbidden in ["activeFocusOnTab", "focus:", "KeyNavigation", "forceActiveFocus"] {
                    assert!(
                        !block.contains(forbidden),
                        "{name}: a `{open}` sets `{forbidden}` (mouse only):\n{block}"
                    );
                }
            }
        }
        assert!(
            !code.contains("activeFocusOnTab"),
            "{name}: sets activeFocusOnTab (Tab itineraries must not change)"
        );
    }
}
