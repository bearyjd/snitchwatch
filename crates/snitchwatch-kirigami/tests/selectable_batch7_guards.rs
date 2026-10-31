//! Selectable text, batch 7 (Prompt and sheets; plan
//! `2026-10-30-selectable-text.md`). Source guards: the class-A texts of the
//! decision and rule sheets, the inspector panel and the connection
//! explanation are `SelectableText`; the texts that stay a `Controls.Label` /
//! `Kirigami.Heading` are exactly the listed, justified ones.
//!
//! The alert window is the security surface and keeps every Label: each
//! `SelectableText` brings its own right-click `Controls.Menu` (a popup that
//! takes keyboard focus while open, so Esc would close it instead of
//! answering Deny and Return would go to its item) and a window-wide Ctrl+C
//! `Shortcut`. The alert's program and path are elided (wrapping them would
//! move the buttons under the pointer) and its countdown is re-written every
//! second. The same data is copyable from the inspector.
//!
//! None of these files has a clickable row (class C), so no `RowCopyMenu`, and
//! no converted text sits inside a click target (`CLICK_TARGETS`) or has one
//! inside it. `selectable_batch7_focus_qml.rs` checks the focus side at runtime.
//!
//! Mouse only: no converted text may add a Tab stop or override
//! `activeFocusOnPress` (false in `SelectableText.qml`). PlainText stays
//! explicit on every converted block (the data here is the connecting
//! program's, the remote host's and the firewall's). Layout unchanged: each
//! converted block's `wrapMode` is pinned per block in `WRAPS`.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "PendingDecisionSheet.qml",
        include_str!("../qml/PendingDecisionSheet.qml"),
        14,
        &[],
    ),
    (
        "MakeRuleSheet.qml",
        include_str!("../qml/MakeRuleSheet.qml"),
        7,
        &[],
    ),
    (
        "LevelRuleActions.qml",
        include_str!("../qml/LevelRuleActions.qml"),
        3,
        &[],
    ),
    (
        "AlertWindow.qml",
        include_str!("../qml/AlertWindow.qml"),
        0,
        &[
            // Security surface: see the module comment.
            "objectName: \"alertProgram\"",
            "objectName: \"alertPath\"",
            "objectName: \"alertDestination\"",
            "text: \"Applies to\"",
            "text: \"For how long\"",
            "objectName: \"alertOnceOnlyNote\"",
            "objectName: \"alertCountdown\"",
        ],
    ),
    (
        "SizedOverlaySheet.qml",
        include_str!("../qml/SizedOverlaySheet.qml"),
        0,
        &[
            // The sheet title elides (honest_ui_pages_qml asserts a long title
            // really does) and its PlainText tooltip is shown on `truncated`.
            "id: titleHeading",
            // The tooltip's content item (class D).
            "color: palette.toolTipText",
        ],
    ),
    (
        "InspectorPanel.qml",
        include_str!("../qml/InspectorPanel.qml"),
        3,
        &[
            // Elided, with a HoverTip on `truncated`; ConnectionDetailsCopy
            // copies the full values.
            "id: titleLabel",
            "objectName: \"inspectorHost\"",
            // Elided; ConnectionDetailsCopy copies them.
            "Kirigami.FormData.label: \"Destination IP\"",
            "Kirigami.FormData.label: \"Matched rule\"",
        ],
    ),
    (
        "ConnectionExplanation.qml",
        include_str!("../qml/ConnectionExplanation.qml"),
        1,
        &[],
    ),
];

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "PendingDecisionSheet.qml",
        &[
            ("\" wants to connect to \"", "Text.Wrap"),
            ("text: \"Scope\"", "Text.NoWrap"),
            ("text: \"Duration\"", "Text.NoWrap"),
            ("id: denyOnceHintLabel", "Text.Wrap"),
            (
                "so this answer applies only to this connection.",
                "Text.Wrap",
            ),
            ("id: oldBridgeNoteLabel", "Text.Wrap"),
            ("\"If nobody answers within \"", "Text.NoWrap"),
            ("can't put a prompt off", "Text.Wrap"),
            ("\"Reverse DNS\"", "Text.Wrap"),
            ("\"Organization\"", "Text.Wrap"),
            ("\"Registrar\"", "Text.Wrap"),
            ("\"Country\"", "Text.Wrap"),
            ("\"unavailable (offline?)\"", "Text.Wrap"),
            ("\"Online research disabled", "Text.Wrap"),
        ],
    ),
    (
        "MakeRuleSheet.qml",
        &[
            ("id: alsoListedNote", "Text.Wrap"),
            ("so it can't make a rule for it.", "Text.Wrap"),
            ("text: \"Scope\"", "Text.NoWrap"),
            ("text: \"Duration\"", "Text.NoWrap"),
            ("id: blockNoteLabel", "Text.Wrap"),
            ("\"makeRuleBusyElsewhere\"", "Text.Wrap"),
            ("\"makeRuleResult\"", "Text.Wrap"),
        ],
    ),
    (
        "LevelRuleActions.qml",
        &[
            ("\"confirmBody\"", "Text.Wrap"),
            ("\"confirmProgram\"", "Text.WrapAnywhere"),
            ("\"ruleMessage\"", "Text.Wrap"),
        ],
    ),
    ("AlertWindow.qml", &[]),
    ("SizedOverlaySheet.qml", &[]),
    (
        "InspectorPanel.qml",
        &[
            ("\"Port\"", "Text.NoWrap"),
            ("\"Protocol\"", "Text.NoWrap"),
            ("\"Verdict\"", "Text.NoWrap"),
        ],
    ),
    (
        "ConnectionExplanation.qml",
        &[("\"connectionExplanation\"", "Text.WrapAnywhere")],
    ),
];

#[test]
fn batch_7_texts_are_selectable() {
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

/// The alert window has no selectable text and no row menu at all (not even
/// outside a block the counter above sees): either brings a focus-taking
/// popup and a Ctrl+C shortcut into the prompt.
#[test]
fn batch_7_alert_window_has_no_selectable_text_or_menu() {
    let code = code_lines(include_str!("../qml/AlertWindow.qml"));
    for forbidden in ["SelectableText", "RowCopyMenu", "Controls.Menu", "Shortcut"] {
        assert!(
            !code.contains(forbidden),
            "AlertWindow.qml uses `{forbidden}`: the prompt must not gain a \
             focus-taking popup or a shortcut"
        );
    }
}

#[test]
fn batch_7_has_no_row_menus() {
    for (name, source, _, _) in FILES {
        assert!(
            blocks(&code_lines(source), "RowCopyMenu {").is_empty(),
            "{name}: no clickable rows in this batch, so no RowCopyMenu"
        );
    }
}

/// Types that react to a click. `Button {` also matches `ToolButton {`,
/// `Controls.Button {` and the like; `ItemDelegate {` also matches
/// `Controls.ItemDelegate {`.
const CLICK_TARGETS: &[&str] = &[
    "ItemDelegate {",
    "MouseArea {",
    "TapHandler {",
    "Button {",
    "AbstractListItem {",
    "BasicListItem {",
    "SwipeListItem {",
];

#[test]
fn batch_7_texts_are_not_inside_click_targets() {
    for (name, source, _, _) in FILES {
        let code = code_lines(source);
        for target in CLICK_TARGETS {
            for block in blocks(&code, target) {
                assert!(
                    !block.contains("SelectableText"),
                    "{name}: a SelectableText sits inside `{target}` (a click on it would \
                     be a click on the row; that needs a RowCopyMenu):\n{block}"
                );
            }
        }
        // And none has a click target of its own inside it.
        for text in blocks(&code, "SelectableText {") {
            for target in CLICK_TARGETS {
                assert!(
                    !text.contains(target),
                    "{name}: a SelectableText contains `{target}`:\n{text}"
                );
            }
        }
    }
}

#[test]
fn batch_7_adds_no_tab_stop() {
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
fn batch_7_text_stays_explicitly_plain() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            let formats: Vec<&str> = block
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("textFormat:"))
                .collect();
            assert_eq!(
                formats,
                ["textFormat: Text.PlainText"],
                "{name}: a SelectableText must set exactly `textFormat: Text.PlainText`:\n{block}"
            );
        }
    }
}

#[test]
fn batch_7_keeps_layout_equivalent() {
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
            let wraps: Vec<&str> = hits[0]
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("wrapMode:"))
                .collect();
            assert_eq!(
                wraps,
                [want.as_str()],
                "{name}: block `{marker}` must have exactly `{want}`:\n{}",
                hits[0]
            );
        }
    }
}
