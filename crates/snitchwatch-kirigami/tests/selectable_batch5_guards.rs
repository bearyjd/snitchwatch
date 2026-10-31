//! Selectable text, batch 5 (Blocklists and geo; plan
//! `2026-10-30-selectable-text.md`). Source guards: the class-A texts of
//! `BlocklistsPage.qml` and `GeoPage.qml` are `SelectableText`; the texts that
//! stay a `Controls.Label` / `Kirigami.Heading` are exactly the listed,
//! justified ones; each clickable row (class C) has one `RowCopyMenu`.
//!
//! Mouse only: no converted text and no row menu may add a Tab stop, and none
//! may override `activeFocusOnPress` (false in `SelectableText.qml`): a click
//! must not steal keyboard focus. Layout unchanged: each converted block's
//! `wrapMode` is pinned per block in `WRAPS`.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "BlocklistsPage.qml",
        include_str!("../qml/BlocklistsPage.qml"),
        16,
        &[
            // Row delegate (class C): the row's RowCopyMenu copies them.
            "text: row.displayName",
            "text: row.url",
            "text: row.entryCount + \" hosts\"",
            "text: row.statusLabel",
            "text: row.enforcementLabel",
            // One host per ListView delegate: a TextEdit there would eat the
            // press that flicks the list.
            "text: host",
        ],
    ),
    (
        "GeoPage.qml",
        include_str!("../qml/GeoPage.qml"),
        2,
        &[
            // Row delegate (class C): the row's RowCopyMenu copies them.
            "text: row.flag",
            "text: row.countryName",
            "text: row.total",
            "text: \"allowed \" + row.allowed",
            "text: \"denied \" + row.denied",
            "text: \"pending \" + row.pending",
        ],
    ),
];

/// RowCopyMenu handlers expected per file (one per clickable row delegate).
const ROW_MENUS: &[(&str, usize)] = &[("BlocklistsPage.qml", 1), ("GeoPage.qml", 1)];

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "BlocklistsPage.qml",
        &[
            ("text: page.title", "Text.NoWrap"),
            ("text: \"Entries\"", "Text.NoWrap"),
            ("\"leftoverText\"", "Text.Wrap"),
            ("\"leftoverConfirmText\"", "Text.Wrap"),
            ("page.leftoverReason", "Text.Wrap"),
            ("page.leftoverFilesLeft", "Text.Wrap"),
            ("exact name, not subdomains", "Text.Wrap"),
            ("page.storageReason", "Text.Wrap"),
            ("page.inspectUrl", "Text.WrapAnywhere"),
            ("page.inspectEntryCount", "Text.NoWrap"),
            ("page.inspectStatusLabel", "Text.NoWrap"),
            ("page.inspectEnforcementLabel", "Text.NoWrap"),
            ("page.inspectEnforcementReason", "Text.Wrap"),
            ("page.inspectLastUpdated", "Text.NoWrap"),
            ("page.inspectLastFailureReason", "Text.Wrap"),
            ("\"Showing \"", "Text.NoWrap"),
        ],
    ),
    (
        "GeoPage.qml",
        &[
            ("geoPageDatabaseSource", "Text.Wrap"),
            ("geoCapacityWarning", "Text.Wrap"),
        ],
    ),
];

/// (file, [(marker unique to one converted Heading block, its exact level)]).
const LEVELS: &[(&str, &[(&str, u32)])] = &[(
    "BlocklistsPage.qml",
    &[("text: page.title", 1), ("text: \"Entries\"", 3)],
)];

#[test]
fn batch_5_headings_keep_their_level() {
    for (name, source, ..) in FILES {
        let converted = blocks(&code_lines(source), "SelectableText {");
        let expected = LEVELS.iter().find(|(n, _)| n == name).map_or(&[][..], |(_, l)| *l);
        let leveled = converted.iter().filter(|b| b.lines().any(|l| l.trim().starts_with("level:"))).count();
        assert_eq!(leveled, expected.len(), "{name}: SelectableText blocks with a `level:`");
        for (marker, level) in expected {
            let hits: Vec<&String> = converted.iter().filter(|b| b.contains(marker)).collect();
            assert_eq!(hits.len(), 1, "{name}: marker `{marker}` must match one block");
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
fn batch_5_texts_are_selectable() {
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
fn batch_5_rows_have_a_copy_menu() {
    for (name, source, _, _) in FILES {
        let code = code_lines(source);
        let expected = ROW_MENUS
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, c)| *c);
        let menus = blocks(&code, "RowCopyMenu {");
        assert_eq!(menus.len(), expected, "{name}: RowCopyMenu blocks");
        for menu in menus {
            assert!(
                menu.contains("copyText:"),
                "{name}: RowCopyMenu without copyText"
            );
        }
    }
}

#[test]
fn batch_5_adds_no_tab_stop() {
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
fn batch_5_external_text_stays_plain() {
    // Texts that show data from outside the GUI keep an explicit PlainText.
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            assert!(
                !block.contains("textFormat: Text.RichText")
                    && !block.contains("textFormat: Text.AutoText")
                    && !block.contains("textFormat: Text.MarkdownText"),
                "{name}: SelectableText must stay PlainText:\n{block}"
            );
        }
    }
}

#[test]
fn batch_5_keeps_layout_equivalent() {
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
