//! Selectable text, batch 4 (Profiles and Recommended rules; plan
//! `2026-10-30-selectable-text.md`). Source guards: the class-A texts of the
//! two pages are `SelectableText` with an exact `wrapMode`; the texts that
//! stay a `Controls.Label` / `Kirigami.Heading` are exactly the listed,
//! justified ones (clickable or copy-menu rows, elided texts, the page title);
//! every row carries a `RowCopyMenu` whose `copyText` names every text the
//! row shows.
//!
//! Mouse only: nothing here may add a Tab stop or take focus. The runtime
//! twin (`selectable_batch4_row_copy_qml.rs`) builds the real rows and checks
//! the copied strings.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks, snippets that identify each text
/// still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "ProfilesPage.qml",
        include_str!("../qml/ProfilesPage.qml"),
        11,
        &[
            // The page title in the title area (the PageRow header is not converted).
            "text: page.title",
            // The profile row is clickable and elided: its Labels stay, the row has the menu.
            "text: row.dotText",
            "text: row.name",
            "text: row.matchersText",
            "text: row.activeText",
            // A rule's action and conditions (elided) share one RowCopyMenu.
            "profileRuleAction",
            "profileRuleConditions",
        ],
    ),
    (
        "RecommendedRulesPage.qml",
        include_str!("../qml/RecommendedRulesPage.qml"),
        4,
        &[
            // Orphan rows: name is elided, the row has one RowCopyMenu, so all its texts stay Labels.
            "orphanName",
            "orphanStatus",
            "orphanProblem",
            "orphanRemoveQuestion",
            // Entry rows: program is elided, the row has one RowCopyMenu, so all its texts stay Labels.
            "text: row.program",
            "allowsLabel",
            "text: row.why",
            "statusLabel",
            "problemLabel",
            "removeQuestion\"",
        ],
    ),
];

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "ProfilesPage.qml",
        &[
            ("notAppliedReason", "Text.Wrap"),
            ("savedNote", "Text.Wrap"),
            ("\"Details: \"", "Text.Wrap"),
            ("objectName: \"profileNote\"", "Text.Wrap"),
            ("FormData.label: \"Status\"", "Text.NoWrap"),
            ("text: \"Rules\"", "Text.NoWrap"),
            ("profileNoRules", "Text.Wrap"),
            ("profileRuleStatus", "Text.NoWrap"),
            ("profileRuleReason", "Text.Wrap"),
            ("profileEditorStatus", "Text.Wrap"),
            ("Delete this profile permanently?", "Text.Wrap"),
        ],
    ),
    (
        "RecommendedRulesPage.qml",
        &[
            ("unavailableReason", "Text.Wrap"),
            ("dnsUnsupported", "Text.Wrap"),
            ("dnsChoiceResult", "Text.Wrap"),
            ("orphansExplanation", "Text.Wrap"),
        ],
    ),
];

/// Per file: one token set per `RowCopyMenu`. Exactly one block must carry
/// each set in full (every text the row shows), and the block count must equal
/// the number of sets.
const COPY_TOKENS: &[(&str, &[&[&str]])] = &[
    (
        "ProfilesPage.qml",
        &[
            &[
                "row.dotText",
                "row.name",
                "row.matchersText",
                "row.activeText",
            ],
            &["ruleRow.modelData.action", "ruleRow.modelData.conditions"],
        ],
    ),
    (
        "RecommendedRulesPage.qml",
        &[
            &[
                "row.program",
                "row.allows",
                "row.why",
                "row.statusText",
                "row.problem",
                "removeQuestion.text",
            ],
            &[
                "orphanRow.modelData.ruleName",
                "orphanRow.modelData.statusText",
                "orphanRow.modelData.problem",
                "orphanRemoveQuestion.text",
            ],
        ],
    ),
];

const FORBIDDEN: &[&str] = &[
    "activeFocusOnTab",
    "activeFocusOnPress",
    "focus:",
    "KeyNavigation",
    "forceActiveFocus",
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
fn batch_4_texts_are_selectable_or_justified() {
    for (name, source, selectable, kept) in FILES {
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
fn batch_4_rows_have_a_copy_menu_with_every_text() {
    for (name, source, ..) in FILES {
        let found = blocks(&code_lines(source), "RowCopyMenu {");
        let (_, sets) = COPY_TOKENS
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name}: missing from COPY_TOKENS"));
        assert_eq!(found.len(), sets.len(), "{name}: RowCopyMenu blocks");
        for block in &found {
            let expr = copy_expression(block)
                .unwrap_or_else(|| panic!("{name}: a RowCopyMenu without copyText:\n{block}"));
            assert!(
                !is_literal_empty(expr),
                "{name}: an empty copyText:\n{block}"
            );
        }
        for set in *sets {
            let hits = found
                .iter()
                .filter(|b| {
                    let expr = copy_expression(b).unwrap_or("");
                    set.iter().all(|t| expr.contains(t))
                })
                .count();
            assert_eq!(
                hits, 1,
                "{name}: exactly one copyText must reference all of {set:?}"
            );
        }
    }
}

#[test]
fn the_copy_text_guard_can_fail() {
    for empty in ["\"\"", "{ return \"\"; }", "{\n  return '';\n}", ""] {
        assert!(is_literal_empty(empty), "`{empty}` should be empty");
    }
    assert!(!is_literal_empty("{ return row.why; }"));
}

#[test]
fn batch_4_adds_no_tab_stop_or_focus() {
    for (name, source, ..) in FILES {
        let code = code_lines(source);
        for open in ["SelectableText {", "RowCopyMenu {"] {
            for block in blocks(&code, open) {
                for forbidden in FORBIDDEN {
                    assert!(
                        !block.contains(forbidden),
                        "{name}: a `{open}` sets `{forbidden}` (mouse only):\n{block}"
                    );
                }
            }
        }
        assert!(
            !code.contains("activeFocusOnTab") && !code.contains("activeFocusOnPress"),
            "{name}: sets activeFocusOnTab/activeFocusOnPress (Tab itineraries must not change)"
        );
    }
}

#[test]
fn batch_4_keeps_layout_equivalent() {
    for (name, source, ..) in FILES {
        let code = code_lines(source);
        let converted = blocks(&code, "SelectableText {");
        for block in &converted {
            assert!(
                !block.contains("elide:"),
                "{name}: a SelectableText sets `elide` (a TextEdit cannot elide):\n{block}"
            );
            assert!(
                !block.contains("textFormat:") || block.contains("textFormat: Text.PlainText"),
                "{name}: a SelectableText overrides PlainText:\n{block}"
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
