//! Selectable text, batch 6 (Rules page; plan `2026-10-30-selectable-text.md`).
//! Source guards: the class-A texts of `RulesPage.qml` (header), the inspector,
//! editor, simulator and import sheets are `SelectableText`; the texts left as
//! a `Controls.Label` are exactly the listed ones (the clickable rule row, class
//! C, where a TextEdit would eat the click); the row has one `RowCopyMenu`
//! whose `copyText` covers every text it shows.
//!
//! Mouse only: no converted text and no row menu may add a Tab stop, and none
//! may override `activeFocusOnPress` (false in `SelectableText.qml`). Layout
//! unchanged: each converted block's `wrapMode` is pinned per block in `WRAPS`.
//! The Menu key (`RulesPage.qml`) is deliberately unchanged (owner decision 2).
//! The copied string itself is checked on a real row in
//! `selectable_batch6_row_copy_qml.rs`.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

const PAGE: &str = include_str!("../qml/RulesPage.qml");

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "RulesPage.qml",
        PAGE,
        10,
        &[
            // The rule row (class C): the row's RowCopyMenu copies them.
            "text: row.ruleEnabled ? \"●\" : \"○\"",
            "(\"blocklist: \" + row.blocklistId)",
            "text: row.operatorSummary",
            "objectName: \"hitsLabel\"",
            "objectName: \"shadowLabel\"",
            "objectName: \"allAppsHint\"",
            "objectName: \"allAppsFlag\"",
            "text: \"read-only\"",
            "text: \"#\" + row.precedence",
            "text: row.ruleAction",
        ],
    ),
    (
        "RulesInspectorSheet.qml",
        include_str!("../qml/RulesInspectorSheet.qml"),
        11,
        &[],
    ),
    (
        "RuleEditorSheet.qml",
        include_str!("../qml/RuleEditorSheet.qml"),
        8,
        &[],
    ),
    (
        "RuleSimulatorSheet.qml",
        include_str!("../qml/RuleSimulatorSheet.qml"),
        13,
        &[],
    ),
    (
        "RulesImportSheet.qml",
        include_str!("../qml/RulesImportSheet.qml"),
        16,
        &[],
    ),
];

/// RowCopyMenu handlers expected per file (one per clickable row delegate).
const ROW_MENUS: &[(&str, usize)] = &[
    ("RulesPage.qml", 1),
    ("RulesInspectorSheet.qml", 0),
    ("RuleEditorSheet.qml", 0),
    ("RuleSimulatorSheet.qml", 0),
    ("RulesImportSheet.qml", 0),
];

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
/// `WrapAnywhere` is where an elided name became a wrapped one (plan class B,
/// case 1: names and paths in sheets).
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "RulesPage.qml",
        &[
            ("text: page.title", "Text.NoWrap"),
            ("objectName: \"allAppsCount\"", "Text.NoWrap"),
            ("objectName: \"hitsSummary\"", "Text.Wrap"),
            ("objectName: \"hitsStorage\"", "Text.Wrap"),
            ("objectName: \"analysisSummary\"", "Text.Wrap"),
            ("objectName: \"rulesIoStatus\"", "Text.Wrap"),
            ("objectName: \"rulesNotShown\"", "Text.Wrap"),
            ("objectName: \"rulesCountHint\"", "Text.Wrap"),
            ("objectName: \"rulesCommandNote\"", "Text.Wrap"),
            ("objectName: \"ruleEditorStatus\"", "Text.Wrap"),
        ],
    ),
    (
        "RulesInspectorSheet.qml",
        &[
            ("FormData.label: \"Name\"", "Text.WrapAnywhere"),
            ("FormData.label: \"Source\"", "Text.NoWrap"),
            ("FormData.label: \"Action\"", "Text.NoWrap"),
            ("FormData.label: \"Duration\"", "Text.NoWrap"),
            ("FormData.label: \"Target\"", "Text.Wrap"),
            ("objectName: \"inspectHowItDecides\"", "Text.Wrap"),
            ("objectName: \"inspectCommandNote\"", "Text.Wrap"),
            ("host.curatedModel.consentResult", "Text.Wrap"),
            ("FormData.label: \"Read-only\"", "Text.Wrap"),
            ("objectName: \"inspectNotEditable\"", "Text.Wrap"),
            ("Delete this rule permanently?", "Text.Wrap"),
        ],
    ),
    (
        "RuleEditorSheet.qml",
        &[
            ("Conditions: the rule matches", "Text.Wrap"),
            (
                "text: \"Match upper and lower case exactly\"",
                "Text.NoWrap",
            ),
            ("objectName: \"editorOperandHelp\"", "Text.Wrap"),
            ("objectName: \"editorProblem\"", "Text.Wrap"),
            ("objectName: \"editorWarning\"", "Text.Wrap"),
            ("objectName: \"editorCaution\"", "Text.Wrap"),
            ("objectName: \"editorRenameNote\"", "Text.Wrap"),
            ("objectName: \"editorStatus\"", "Text.Wrap"),
        ],
    ),
    (
        "RuleSimulatorSheet.qml",
        &[
            ("Evaluates a candidate connection", "Text.Wrap"),
            ("Regular expressions are matched", "Text.Wrap"),
            ("Started from the connection you chose", "Text.Wrap"),
            ("Everything else opensnitchd can match on", "Text.Wrap"),
            ("Once you type a variable here", "Text.Wrap"),
            ("Simulated result: not a live daemon verdict.", "Text.Wrap"),
            ("\"Matched: \"", "Text.Wrap"),
            ("text: \"Action: \"", "Text.NoWrap"),
            ("What happens next depends on the daemon", "Text.Wrap"),
            ("text: sheet.simulateWarnings", "Text.Wrap"),
            (
                "Not evaluated, because an input was left blank",
                "Text.Wrap",
            ),
            ("Not evaluated, because an input isn't valid", "Text.Wrap"),
            ("Can't simulate these conditions", "Text.Wrap"),
        ],
    ),
    (
        "RulesImportSheet.qml",
        &[
            ("objectName: \"importRowName\"", "Text.WrapAnywhere"),
            ("objectName: \"importCaution\"", "Text.Wrap"),
            ("objectName: \"importBadge\"", "Text.NoWrap"),
            ("objectName: \"importDetail\"", "Text.Wrap"),
            ("objectName: \"importPreviousHeading\"", "Text.NoWrap"),
            ("objectName: \"importPrevious\"", "Text.Wrap"),
            ("objectName: \"importResult\"", "Text.Wrap"),
            ("text: plain.row.displayName", "Text.WrapAnywhere"),
            ("objectName: \"importProblem\"", "Text.Wrap"),
            ("Nothing is sent until you apply", "Text.Wrap"),
            ("objectName: \"importSectionAdd\"", "Text.NoWrap"),
            ("objectName: \"importSectionReplace\"", "Text.NoWrap"),
            ("objectName: \"importSectionRefused\"", "Text.NoWrap"),
            ("objectName: \"importSectionUnchanged\"", "Text.NoWrap"),
            ("objectName: \"importStatus\"", "Text.Wrap"),
            ("objectName: \"importTooMany\"", "Text.Wrap"),
        ],
    ),
];

#[test]
fn batch_6_texts_are_selectable() {
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
                "{name}: expected a kept Label/Heading containing `{snippet}`"
            );
        }
    }
}

#[test]
fn batch_6_page_title_keeps_its_heading_level() {
    let code = code_lines(PAGE);
    let titles: Vec<String> = blocks(&code, "SelectableText {")
        .into_iter()
        .filter(|b| b.contains("text: page.title"))
        .collect();
    assert_eq!(titles.len(), 1, "one page-title SelectableText");
    assert!(
        titles[0].lines().any(|l| l.trim() == "level: 1"),
        "the old Kirigami.Heading was level 1:\n{}",
        titles[0]
    );
    // Only the title was a heading; no other block may claim a level.
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            if block.contains("text: page.title") {
                continue;
            }
            assert!(
                !block.contains("level:"),
                "{name}: unexpected `level:` on a body text:\n{block}"
            );
        }
    }
}

#[test]
fn batch_6_rows_have_a_copy_menu() {
    for (name, source, _, _) in FILES {
        let code = code_lines(source);
        let expected = ROW_MENUS
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, c)| *c);
        let menus = blocks(&code, "RowCopyMenu {");
        assert_eq!(menus.len(), expected, "{name}: RowCopyMenu blocks");
        for menu in menus {
            let value = menu
                .lines()
                .find_map(|l| l.trim().strip_prefix("copyText:"))
                .unwrap_or_else(|| panic!("{name}: RowCopyMenu without copyText"))
                .trim();
            assert!(
                !value.is_empty() && value != "\"\"" && value != "''" && value.contains("row."),
                "{name}: copyText must be built from the row, not empty/literal: `{value}`"
            );
        }
    }
}

/// The row's copy function names every text property the row's Labels show
/// (the runtime probe in `selectable_batch6_row_copy_qml.rs` checks the string).
#[test]
fn batch_6_row_copy_text_names_every_shown_text() {
    let code = code_lines(PAGE);
    let start = code
        .find("function copyLines()")
        .expect("the delegate has a copyLines() function");
    let end = start
        + code[start..]
            .find("contentItem:")
            .expect("copyLines() sits before contentItem");
    let body = &code[start..end];
    for needed in [
        "row.ruleEnabled",
        "\"●\"",
        "\"○\"",
        "\"blocklist: \" + row.blocklistId",
        "row.displayName",
        "row.operatorSummary",
        "insightsText.hitsRowText(",
        "row.shadowText",
        "row.allAppsHint",
        "row.flagBadge",
        "\"read-only\"",
        "\"#\" + row.precedence",
        "row.ruleAction",
    ] {
        assert!(
            body.contains(needed),
            "copyLines() lacks `{needed}`:\n{body}"
        );
    }
}

/// Owner decision 2: the Menu key still opens the inspector and nothing else.
#[test]
fn batch_6_menu_key_is_unchanged() {
    let code = code_lines(PAGE);
    assert_eq!(
        code.matches("Keys.onMenuPressed: list.openCurrent()")
            .count(),
        1
    );
    assert_eq!(code.matches("Keys.onMenuPressed").count(), 1);
}

#[test]
fn batch_6_adds_no_tab_stop() {
    for (name, source, _, _) in FILES {
        let code = code_lines(source);
        for open in ["SelectableText {", "RowCopyMenu {"] {
            for block in blocks(&code, open) {
                for forbidden in [
                    "activeFocusOnTab",
                    "focus:",
                    "KeyNavigation",
                    "forceActiveFocus",
                    "activeFocusOnPress",
                ] {
                    assert!(
                        !block.contains(forbidden),
                        "{name}: a `{open}` sets `{forbidden}` (mouse only):\n{block}"
                    );
                }
            }
        }
    }
}

#[test]
fn batch_6_external_text_stays_plain() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            assert!(
                !block.contains("textFormat: Text.RichText")
                    && !block.contains("textFormat: Text.AutoText")
                    && !block.contains("textFormat: Text.StyledText")
                    && !block.contains("textFormat: Text.MarkdownText"),
                "{name}: SelectableText must stay PlainText:\n{block}"
            );
        }
    }
}

#[test]
fn batch_6_keeps_layout_equivalent() {
    for (name, source, _, _) in FILES {
        let converted = blocks(&code_lines(source), "SelectableText {");
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
