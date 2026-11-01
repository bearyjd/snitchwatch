//! Selectable text, batch 1 (Status pages; plan
//! `2026-10-30-selectable-text.md`). Source guards: every class-A text of the
//! batch's files is a `SelectableText`, and the few texts that stay a
//! `Controls.Label` / `Kirigami.Heading` are exactly the listed, justified
//! ones. A page that grows a new plain Label, or loses a conversion, fails.
//!
//! Mouse only: converted texts must not add a Tab stop, so no
//! `activeFocusOnTab` / `focus` override may appear in these files either.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block, in any order).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "DaemonHealthPage.qml",
        include_str!("../qml/DaemonHealthPage.qml"),
        4,
        &[],
    ),
    // Left: the Crash log TextArea (class F, not a Label) and the heading row's
    // Button/BusyIndicator are not text items. Headings and Labels all convert.
    (
        "DiagnosticsPage.qml",
        include_str!("../qml/DiagnosticsPage.qml"),
        7,
        &[],
    ),
    (
        "ScannerPage.qml",
        include_str!("../qml/ScannerPage.qml"),
        5,
        &[],
    ),
    (
        "DenyStatusSection.qml",
        include_str!("../qml/DenyStatusSection.qml"),
        7,
        &[],
    ),
    // The two short field captions ("Manual ... fallback location", "Current
    // database") are converted too.
    (
        "GeoDatabaseSettings.qml",
        include_str!("../qml/GeoDatabaseSettings.qml"),
        15,
        &[],
    ),
    (
        "TrafficPage.qml",
        include_str!("../qml/TrafficPage.qml"),
        3,
        &[],
    ),
    ("MapView.qml", include_str!("../qml/MapView.qml"), 2, &[]),
];

/// Converted `Kirigami.Heading` blocks per file (all were level 3).
const HEADINGS: &[(&str, usize)] = &[
    ("DiagnosticsPage.qml", 2),
    ("ScannerPage.qml", 1),
    ("DenyStatusSection.qml", 1),
    ("GeoDatabaseSettings.qml", 1),
];

#[test]
fn batch_1_texts_are_selectable() {
    for (name, source, selectable, kept) in FILES {
        let code = code_lines(source);
        let converted = blocks(&code, "SelectableText {").len();
        assert_eq!(converted, *selectable, "{name}: SelectableText blocks");
        // Each converted Heading keeps its Kirigami level (size + Heading role).
        let leveled = blocks(&code, "SelectableText {")
            .iter()
            .filter(|b| b.lines().any(|l| l.trim() == "level: 3"))
            .count();
        let expected = HEADINGS
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, c)| *c);
        assert_eq!(
            leveled, expected,
            "{name}: SelectableText blocks with `level: 3`"
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
fn batch_1_adds_no_tab_stop() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            for forbidden in [
                "activeFocusOnTab",
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
    }
}
