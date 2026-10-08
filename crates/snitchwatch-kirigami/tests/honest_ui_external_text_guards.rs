//! Issue #51 follow-up: text produced outside the GUI (the bridge runtime's
//! error messages, the scanner's stderr and the host file paths it reports,
//! daemon diagnostics, wizard and autostart errors, the crash log) is never
//! rendered as markup. `Controls.Label` defaults to AutoText, and
//! `Kirigami.InlineMessage` has no `textFormat` hook at all, so such text sits
//! in a PlainText label, never in an InlineMessage.
//!
//! The same crude source checks as `honest_ui_qml_guards.rs`, which covers
//! the Rules, Connections, Blocklists and Profiles pages and the decision
//! sheet; this file covers the pages and banners that one left out.
//! `honest_ui_external_text_qml.rs` loads the edited pages with markup in
//! that text.

const PAGES: &[(&str, &str, &[&str], usize)] = &[
    (
        "main.qml",
        include_str!("../qml/main.qml"),
        &["bridgeFeed.statusText"],
        1,
    ),
    (
        "DaemonHealthPage.qml",
        include_str!("../qml/DaemonHealthPage.qml"),
        &["troubleshootingText"],
        1,
    ),
    (
        "DiagnosticsPage.qml",
        include_str!("../qml/DiagnosticsPage.qml"),
        &["autostartError", "coexistenceDetail"],
        2,
    ),
    (
        "ScannerPage.qml",
        include_str!("../qml/ScannerPage.qml"),
        &["modelData.path", "errorText"],
        2,
    ),
    (
        "OnboardingPage.qml",
        include_str!("../qml/OnboardingPage.qml"),
        &["controller.detail"],
        1,
    ),
    (
        "GeoPage.qml",
        include_str!("../qml/GeoPage.qml"),
        &["row.countryName"],
        1,
    ),
];

/// Drop whole-line `//` comments.
fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `<open> ... }` body, by brace counting outside double-quoted strings.
fn blocks(source: &str, open: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(open) {
        let body_start = start + open.len();
        let mut depth = 1usize;
        let mut in_string = false;
        let mut end = rest.len();
        for (i, ch) in rest[body_start..].char_indices() {
            match ch {
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        found.push(rest[body_start..end].to_string());
        rest = &rest[end..];
    }
    found
}

/// The block's own `text:` binding (at its top property indent) plus deeper
/// continuation lines.
fn text_binding(block: &str) -> Option<String> {
    let lines: Vec<&str> = block.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(lines.iter().find(|l| !l.trim().is_empty())?);
    let start = lines
        .iter()
        .position(|l| indent(l) == base && l.trim_start().starts_with("text:"))?;
    let mut binding = lines[start].to_string();
    for line in &lines[start + 1..] {
        if line.trim().is_empty() || indent(line) <= base {
            break;
        }
        binding.push('\n');
        binding.push_str(line);
    }
    Some(binding)
}

#[test]
fn external_text_is_shown_in_plain_text_labels() {
    for (name, source, exprs, min_checked) in PAGES {
        let mut checked = 0;
        for block in blocks(&code_lines(source), "Controls.Label {") {
            let Some(binding) = text_binding(&block) else {
                continue;
            };
            if !exprs.iter().any(|e| binding.contains(e)) {
                continue;
            }
            checked += 1;
            assert!(
                block.contains("textFormat: Text.PlainText"),
                "{name} shows external text in a label without `textFormat: Text.PlainText` \
                 (issue #51):\n{block}"
            );
        }
        assert!(
            checked >= *min_checked,
            "expected at least {min_checked} labels showing {exprs:?} in {name}, found {checked}"
        );
    }
}

#[test]
fn external_text_never_goes_into_an_inline_message() {
    for (name, source, exprs, _) in PAGES {
        for block in blocks(&code_lines(source), "Kirigami.InlineMessage {") {
            let binding = text_binding(&block).unwrap_or_default();
            assert!(
                !exprs.iter().any(|e| binding.contains(e)),
                "{name} puts external text in an InlineMessage, which renders it as markup; use \
                 fixed text there and a PlainText label beside it (issue #51):\n{binding}"
            );
        }
    }
}

#[test]
fn the_crash_log_is_plain_text() {
    let page = code_lines(include_str!("../qml/DiagnosticsPage.qml"));
    let areas: Vec<String> = blocks(&page, "Controls.TextArea {")
        .into_iter()
        .filter(|b| b.contains("crashLogText"))
        .collect();
    assert_eq!(areas.len(), 1, "the crash-log TextArea moved");
    assert!(
        areas[0].contains("textFormat: TextEdit.PlainText"),
        "the crash log must be shown as plain text, not left to the default:\n{}",
        areas[0]
    );
}
