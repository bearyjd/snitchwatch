//! Selectable text, batch 8 (Connections and map; plan
//! `2026-10-30-selectable-text.md`). Source guards: the class-A texts of the
//! batch's files are `SelectableText`; the texts that stay a
//! `Controls.Label` / `Kirigami.Heading` are exactly the listed, justified
//! ones.
//!
//! The Connections row (class C) gets no `RowCopyMenu`: its right button
//! already opens the row's actions menu (`ConnectionActions.qml`, which the
//! Menu key / Shift+F10 opens too, owner decision 2), so a second right-click
//! handler would open two menus. Its right-click Copy is one item in that menu
//! instead ("Copy row text"). The text is built by that one menu object
//! (`ConnectionActions.rowText(row)`) only when it opens: the list is
//! virtualised and reuses delegates, so the row carries no extra object and
//! no extra binding (delegate creation cost stays flat).
//!
//! Mouse only: no converted text may take focus or add a Tab stop.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

const ROW: &str = include_str!("../qml/ConnectionRow.qml");
const LIST: &str = include_str!("../qml/ConnectionList.qml");
const ACTIONS: &str = include_str!("../qml/ConnectionActions.qml");

/// (file, source, SelectableText blocks, snippets that identify each text
/// still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    // Class C: every Label sits on the row's click/right-click MouseArea (a
    // TextEdit would eat the press that selects, opens or toggles the row,
    // and the list's flick); the host line is elided. "Copy row text" in the
    // row's actions menu copies them all, full length. Two are tooltips (D).
    (
        "ConnectionRow.qml",
        ROW,
        0,
        &[
            "page.verdictGlyph(",
            "objectName: \"hostLabel\"",
            "objectName: \"heldBackLabel\"",
            "objectName: \"verdictLabel\"",
            "text: row.actions.totalText(row)",
            "text: row.denyAllText",
            "text: row.denyText",
        ],
    ),
    // Only a Kirigami PlaceholderMessage (class E) and the row delegate.
    ("ConnectionList.qml", LIST, 0, &[]),
    ("ConnectionsHeader.qml", include_str!("../qml/ConnectionsHeader.qml"), 1, &[]),
    ("ConnectionsFilterBar.qml", include_str!("../qml/ConnectionsFilterBar.qml"), 1, &[]),
    // On the row's click target, and both elided; the row's copy has them.
    (
        "ProgramLabel.qml",
        include_str!("../qml/ProgramLabel.qml"),
        0,
        &["part.src.programLabel", "part.src.programPathTail"],
    ),
    // Kept: the chevron's tooltip (D); two elided header lines (the
    // destination count and the geographic scope, both ElideRight in a row
    // that cannot grow a line); the footer's two wrapping texts (a wrapping
    // TextEdit's implicitHeight differs from a Label's and feeds the band's
    // `chromeHeight`, i.e. the SplitView sizes: converting them breaks
    // `map_split_drag_real_qml`).
    (
        "MapBand.qml",
        include_str!("../qml/MapBand.qml"),
        5,
        &[
            "MapBandState.collapsed ? \"Show map\"",
            "no destinations yet",
            "map.activeGeoScope.name",
            "observed cached connections",
            "id: honestText",
        ],
    ),
    // The marker is a button on the draggable map (D): its glyph, its
    // elided name and its tooltip stay Labels.
    (
        "MapMarker.qml",
        include_str!("../qml/MapMarker.qml"),
        0,
        &["marker.glyph", "marker.cluster.members[0].name", "marker.cluster.summary"],
    ),
    // Both sit (z: 1) on a group header's toggle/right-click MouseArea, and
    // the header already uses right-click for its actions: kept, and copied
    // by the header's "Copy row text".
    ("GroupPendingBadge.qml", include_str!("../qml/GroupPendingBadge.qml"), 0, &["badge.count"]),
    (
        "GroupVerdictCounts.qml",
        include_str!("../qml/GroupVerdictCounts.qml"),
        0,
        &["piece.modelData.label", "piece.modelData.text"],
    ),
];

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    ("ConnectionRow.qml", &[]),
    ("ConnectionList.qml", &[]),
    ("ConnectionsHeader.qml", &[("header.title", "Text.NoWrap")]),
    ("ConnectionsFilterBar.qml", &[("\"Filter: \"", "Text.WordWrap")]),
    ("ProgramLabel.qml", &[]),
    (
        "MapBand.qml",
        &[
            ("text: \"Map\"", "Text.NoWrap"),
            ("● allowed", "Text.NoWrap"),
            ("■ denied", "Text.NoWrap"),
            ("◆ pending", "Text.NoWrap"),
            ("ring: mixed verdicts", "Text.NoWrap"),
        ],
    ),
    ("MapMarker.qml", &[]),
    ("GroupPendingBadge.qml", &[]),
    ("GroupVerdictCounts.qml", &[]),
];

/// `activeFocusOnTab` occurrences each file had before this batch (the list
/// itself and the map marker button): the batch must not add or drop one.
const TAB_STOPS: &[(&str, usize)] = &[("ConnectionList.qml", 1), ("MapMarker.qml", 1)];

/// What the row's copy text must be built from: every text the row shows,
/// leaf and header, through the same helpers its Labels show.
const COPY_TOKENS: &[&str] = &[
    "actions.page.verdictGlyph(row.verdict, row.pending)",
    "row.programLabel",
    "row.programLabelUnverified",
    "row.programPathFull",
    "actions.hostLine(row)",
    "row.pending && row.retriedCount > 0 ? [actions.heldBackText(row)]",
    "actions.verdictText(row)",
    "actions.totalText(row)",
    "row.groupPending > 0 ? [row.groupPending + \" pending\"]",
    "row.verdictCountsItem.summary",
];

/// The body of `function <name>(...) { ... }` in `source`.
fn function_body(source: &str, name: &str) -> Option<String> {
    let at = source.find(&format!("function {name}("))?;
    blocks(&source[at..], ") {").into_iter().next()
}

/// A function body that only returns nothing or a literal empty string.
fn is_literal_empty(body: &str) -> bool {
    let bare: String = body
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '{' | '}' | ';'))
        .collect();
    let bare = bare.strip_prefix("return").unwrap_or(&bare);
    matches!(bare, "" | "\"\"" | "''" | "``")
}

#[test]
fn batch_8_texts_are_selectable_or_justified() {
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
fn batch_8_adds_no_tab_stop_and_no_focus() {
    for (name, source, ..) in FILES {
        let code = code_lines(source);
        for block in blocks(&code, "SelectableText {") {
            for forbidden in [
                "activeFocusOnTab",
                "activeFocusOnPress",
                "focus:",
                "KeyNavigation",
                "forceActiveFocus",
            ] {
                assert!(
                    !block.contains(forbidden),
                    "{name}: a SelectableText sets `{forbidden}` (mouse only):\n{block}"
                );
            }
        }
        let want = TAB_STOPS.iter().find(|(n, _)| n == name).map_or(0, |(_, c)| *c);
        assert_eq!(
            code.matches("activeFocusOnTab").count(),
            want,
            "{name}: activeFocusOnTab count changed (Tab itineraries must not change)"
        );
    }
}

#[test]
fn batch_8_keeps_layout_equivalent_and_plain_text() {
    for (name, source, ..) in FILES {
        let code = code_lines(source);
        let converted = blocks(&code, "SelectableText {");
        for block in &converted {
            assert!(
                !block.contains("elide:"),
                "{name}: a SelectableText sets `elide` (a TextEdit cannot elide):\n{block}"
            );
            assert!(
                block.lines().any(|l| l.trim() == "textFormat: Text.PlainText"),
                "{name}: a SelectableText without `textFormat: Text.PlainText`:\n{block}"
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

/// The page title keeps its Kirigami.Heading size (level 1).
#[test]
fn batch_8_heading_keeps_its_level() {
    let code = code_lines(include_str!("../qml/ConnectionsHeader.qml"));
    let title = blocks(&code, "SelectableText {");
    assert!(
        title.len() == 1 && title[0].lines().any(|l| l.trim() == "level: 1"),
        "ConnectionsHeader.qml: the title is not a level-1 SelectableText:\n{title:?}"
    );
}

/// Owner decision 2 / plan flag: the Menu key and Shift+F10 still open the
/// row's actions, exactly as before.
#[test]
fn batch_8_menu_key_opens_the_row_actions_unchanged() {
    let code = code_lines(LIST);
    for needed in [
        "const menuKey = event.key === Qt.Key_Menu",
        "|| (event.key === Qt.Key_F10 && (event.modifiers & Qt.ShiftModifier));",
        "pane.page.connectionActions.open(row, 0, row.height);",
    ] {
        assert!(code.contains(needed), "ConnectionList.qml lost: {needed}");
    }
    // The row's right button still opens the same actions menu.
    let row = code_lines(ROW);
    assert!(row.contains("acceptedButtons: Qt.LeftButton | Qt.RightButton"));
    assert!(row.contains("row.actions.open(row, mouse.x, mouse.y);"));
}

/// Performance: the list is virtualised and reuses delegates, so the row gets
/// no new object, popup, loader or binding for its copy. The text is a
/// function the actions menu calls once, when it opens.
#[test]
fn batch_8_row_copy_costs_nothing_per_delegate() {
    let row = code_lines(ROW);
    for forbidden in [
        "RowCopyMenu {",
        "SelectableText {",
        "Loader {",
        "Component {",
        "Menu {",
        "TapHandler {",
        "rowText",
        "property string copyText",
    ] {
        assert!(!row.contains(forbidden), "ConnectionRow.qml: `{forbidden}` adds per-row cost");
    }
    let list = code_lines(LIST);
    let delegate = blocks(&list, "delegate: ConnectionRow {");
    assert_eq!(delegate.len(), 1, "ConnectionList.qml: the row delegate moved");
    assert_eq!(
        delegate[0].split_whitespace().collect::<Vec<_>>(),
        ["page:", "pane.page"],
        "ConnectionList.qml: the row delegate changed"
    );
    for file in [
        include_str!("../qml/ProgramLabel.qml"),
        include_str!("../qml/GroupPendingBadge.qml"),
        include_str!("../qml/GroupVerdictCounts.qml"),
    ] {
        let code = code_lines(file);
        assert!(!code.contains("RowCopyMenu {") && !code.contains("SelectableText {"));
    }
}

/// The row's copy text (`ConnectionActions.rowText(row)`): built from every
/// text the row shows, never empty, and the row's Labels show the same
/// helpers (so the copy cannot drift from the screen).
#[test]
fn batch_8_row_copy_text_has_every_shown_text() {
    let actions = code_lines(ACTIONS);
    let body = function_body(&actions, "rowText").expect("ConnectionActions.qml: no rowText(row)");
    assert!(!is_literal_empty(&body), "rowText(row) is empty:\n{body}");
    for token in COPY_TOKENS {
        assert!(body.contains(token), "rowText(row) does not use `{token}`:\n{body}");
    }
    let row = code_lines(ROW);
    for shown in [
        "text: row.actions.hostLine(row)",
        "text: row.actions.heldBackText(row)",
        "text: row.actions.verdictText(row)",
        "text: row.actions.totalText(row)",
    ] {
        assert_eq!(row.matches(shown).count(), 1, "ConnectionRow.qml: a Label no longer shows `{shown}`");
    }
    assert!(row.contains("property alias verdictCountsItem: verdictCounts"));
    for helper in ["hostLine", "heldBackText", "verdictText", "totalText"] {
        let body = function_body(&actions, helper).unwrap_or_else(|| panic!("no {helper}(row)"));
        assert!(!is_literal_empty(&body), "{helper}(row) is empty");
    }
}

/// The actions menu: one "Copy row text" item, captured when the menu
/// opened (delegates are reused while it is open), gated like the other copy
/// items, hidden on the rule button's menu, never a focus override.
#[test]
fn batch_8_actions_menu_copies_the_row_text() {
    let code = code_lines(ACTIONS);
    assert!(
        code.contains("rowText: actions.rowText(row),"),
        "ConnectionActions.snapshot() does not capture the row text"
    );
    let items: Vec<String> = blocks(&code, "GuardedMenuItem {")
        .into_iter()
        .filter(|b| b.contains("objectName: \"copyRowText\""))
        .collect();
    assert_eq!(items.len(), 1, "ConnectionActions.qml: copyRowText items");
    let item = &items[0];
    for needed in [
        "visible: !actions.rulesOnly && !!actions.target",
        "enabled: actions.valid() && actions.target.rowText !== \"\"",
        "onTriggered: if (actions.valid()) clipboard.copy(actions.target.rowText)",
    ] {
        assert!(item.contains(needed), "copyRowText lacks `{needed}`:\n{item}");
    }
    for forbidden in ["activeFocusOnTab", "activeFocusOnPress", "focus:", "forceActiveFocus"] {
        assert!(!item.contains(forbidden), "copyRowText sets `{forbidden}`");
    }
}

#[test]
fn the_copy_text_guard_can_fail() {
    for empty in ["\"\"", "{ return \"\"; }", "{\n  return '';\n}", ""] {
        assert!(is_literal_empty(empty), "`{empty}` should be empty");
    }
    assert!(!is_literal_empty("{ return row.hostLine(); }"));
    assert_eq!(
        function_body("function f(a) {\n    return a;\n}\n", "f").as_deref().map(str::trim),
        Some("return a;")
    );
}
