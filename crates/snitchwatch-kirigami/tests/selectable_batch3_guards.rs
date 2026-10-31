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
            "row.dualUseText",
            "row.highFanOutText",
            "row.hostLine(modelData)",
            "id: notSelectable",
            "row.alreadyPresentText",
            "modelData.title",
            "row.onlyText(modelData)",
            "row.noteText(modelData)",
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

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "LearningReviewPage.qml",
        &[
            ("Programs recorded:", "Text.Wrap"),
            ("Some connections were missed", "Text.Wrap"),
            ("Programs not recorded or dropped", "Text.Wrap"),
            ("group of programs with flags is full", "Text.Wrap"),
            ("Folders that hold more programs", "Text.Wrap"),
            ("rules list could not be read", "Text.Wrap"),
            ("controller.fileNotice", "Text.Wrap"),
            ("This deletes everything recorded", "Text.Wrap"),
            ("controller.statusText", "Text.Wrap"),
            ("Nothing has been recorded.", "Text.Wrap"),
        ],
    ),
    ("LearningReviewRow.qml", &[]),
    (
        "LearningProgressList.qml",
        &[
            ("summaryLabel", "Text.Wrap"),
            ("modelData.label", "Text.WrapAnywhere"),
            ("modelData.status", "Text.WrapAnywhere"),
        ],
    ),
    ("LearningLimitsList.qml", &[("text: modelData", "Text.Wrap")]),
    (
        "CuratedGroupsStep.qml",
        &[
            ("Optional. Each switch adds", "Text.Wrap"),
            ("cannot accept reviewed DNS consent", "Text.Wrap"),
            ("consentResult", "Text.Wrap"),
        ],
    ),
];

/// Per file with a `RowCopyMenu`: what its `copyText` expression must
/// reference (every text the row shows, so the copy is the whole record).
const COPY_TOKENS: &[(&str, &[&str])] = &[
    (
        "LearningReviewRow.qml",
        &[
            "i.programLabel",
            "i.programPathFull",
            "row.countsText",
            "i.flags",
            "row.dualUseText",
            "row.highFanOutText",
            "row.hostLine(",
            "i.notSelectable",
            "row.alreadyPresentText",
            "c.title",
            "row.onlyText(",
            "row.noteText(",
        ],
    ),
    (
        "CuratedGroupsStep.qml",
        &[
            "modelData.title",
            "modelData.description",
            "modelData.entries",
            "e.program",
            "e.allows",
            "e.why",
            "e.statusText",
        ],
    ),
];

/// The `copyText:` binding of a `RowCopyMenu` block, to the end of the block.
fn copy_expression(block: &str) -> Option<&str> {
    let at = block.find("copyText:")?;
    Some(block[at + "copyText:".len()..].trim())
}

/// A copy expression that is empty or only a literal empty string.
fn is_literal_empty(expr: &str) -> bool {
    let bare: String = expr
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '{' | '}' | ';'))
        .collect();
    let bare = bare.strip_prefix("return").unwrap_or(&bare);
    matches!(bare, "" | "\"\"" | "''" | "``")
}

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
        let tokens = COPY_TOKENS.iter().find(|(n, _)| n == name).map(|(_, t)| *t);
        assert_eq!(tokens.is_some(), *menus > 0, "{name}: COPY_TOKENS entry");
        for block in found {
            let expr = copy_expression(&block)
                .unwrap_or_else(|| panic!("{name}: a RowCopyMenu without copyText:\n{block}"));
            assert!(!is_literal_empty(expr), "{name}: an empty copyText:\n{block}");
            for token in tokens.unwrap_or(&[]) {
                assert!(
                    expr.contains(token),
                    "{name}: copyText does not reference `{token}`:\n{block}"
                );
            }
        }
    }
}

#[test]
fn the_copy_text_guard_can_fail() {
    for empty in ["\"\"", "{ return \"\"; }", "{\n  return '';\n}", ""] {
        assert!(is_literal_empty(empty), "`{empty}` should be empty");
    }
    assert!(!is_literal_empty("{ return row.dualUseText; }"));
}

#[test]
fn batch_3_adds_no_tab_stop() {
    for (name, source, ..) in FILES {
        let code = code_lines(source);
        for open in ["SelectableText {", "RowCopyMenu {"] {
            for block in blocks(&code, open) {
                for forbidden in ["activeFocusOnTab", "activeFocusOnPress", "focus:", "KeyNavigation", "forceActiveFocus"] {
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

#[test]
fn batch_3_keeps_layout_equivalent() {
    for (name, source, ..) in FILES {
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
        assert_eq!(converted.len(), expected.len(), "{name}: WRAPS entries vs blocks");
        for (marker, wrap) in *expected {
            let hits: Vec<&String> = converted.iter().filter(|b| b.contains(marker)).collect();
            assert_eq!(hits.len(), 1, "{name}: marker `{marker}` must match one block");
            let want = format!("wrapMode: {wrap}");
            assert!(
                hits[0].lines().any(|l| l.trim() == want),
                "{name}: block `{marker}` must have `{want}`:\n{}",
                hits[0]
            );
        }
    }
}
