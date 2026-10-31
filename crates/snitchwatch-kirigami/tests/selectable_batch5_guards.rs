//! Selectable text, batch 5 (Blocklists and geo; plan
//! `2026-10-30-selectable-text.md`). Source guards: the class-A texts of
//! `BlocklistsPage.qml` and `GeoPage.qml` are `SelectableText`; the texts that
//! stay a `Controls.Label` / `Kirigami.Heading` are exactly the listed,
//! justified ones; each clickable row (class C) has one `RowCopyMenu`.
//!
//! Mouse only: no converted text and no row menu may add a Tab stop.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "BlocklistsPage.qml",
        include_str!("../qml/BlocklistsPage.qml"),
        14,
        &[
            // Page title and the sheet's "Entries" caption: Kirigami.Heading
            // (SelectableText has no `level` on this branch; batch 1 owns it).
            "text: page.title",
            "text: \"Entries\"",
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
        let expected = ROW_MENUS.iter().find(|(n, _)| n == name).map_or(0, |(_, c)| *c);
        let menus = blocks(&code, "RowCopyMenu {");
        assert_eq!(menus.len(), expected, "{name}: RowCopyMenu blocks");
        for menu in menus {
            assert!(menu.contains("copyText:"), "{name}: RowCopyMenu without copyText");
        }
    }
}

#[test]
fn batch_5_adds_no_tab_stop() {
    for (name, source, _, _) in FILES {
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
