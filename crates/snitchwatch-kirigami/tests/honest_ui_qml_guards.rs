//! Source-level guards for the "honest UI" fixes: surfaces that must not claim
//! more than the bridge actually delivers, and labels that must not render
//! daemon-derived strings as markup.
//!
//! Deliberately crude text assertions, for the same reason as
//! `qml_source_guards.rs`: cxx-qt-lib exposes no way to inspect a rendered
//! `Label`'s `textFormat` or an `InlineMessage`'s `showCloseButton` from Rust,
//! and `qmltestrunner` cannot load `com.snitchwatch.shell`. These keep a
//! regression from silently re-introducing what the fixes removed;
//! `honest_ui_pages_qml.rs` separately proves the edited pages still load.

mod qml_guard_support;
use qml_guard_support::*;

/// `Kirigami.InlineMessage` renders its `text` through a `SelectableLabel`
/// (AutoText) with no `textFormat` hook, and HTML-escaping doesn't fix that:
/// AutoText only decodes entities when it already judges the string to be
/// markup, so an escaped `&lt;b&gt;` shows up literally. Data therefore never
/// goes into an InlineMessage — only string literals joined by `+` do.
///
/// Scope: the decision prompt, Connections, Blocklists, Profiles and Rules.
/// `honest_ui_external_text_guards.rs` keeps the external text on `main.qml`
/// and the diagnostics pages out of InlineMessages; the banners left there
/// (`statusSummary`, the pending-age count) show only text built locally.
#[test]
fn inline_messages_carry_only_fixed_text() {
    let mut checked = 0;
    for (name, source) in [
        ("PendingDecisionSheet.qml", PENDING_SHEET),
        ("ConnectionsPage.qml", CONNECTIONS_PAGE),
        ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
        ("ProfilesPage.qml", PROFILES_PAGE),
        ("RecommendedRulesPage.qml", RECOMMENDED_PAGE),
        ("RulesPage.qml", RULES_PAGE),
        ("RuleSimulatorSheet.qml", SIMULATOR_SHEET),
        ("RulesImportSheet.qml", IMPORT_SHEET),
        ("RuleEditorSheet.qml", EDITOR_SHEET),
    ] {
        for block in blocks(&code_lines(source), "Kirigami.InlineMessage {") {
            checked += 1;
            let binding = text_binding(&block).unwrap_or_default();
            assert!(
                is_fixed_text(&binding),
                "{name} has an InlineMessage whose text is not purely string literals joined by \
                 `+`. InlineMessage can't render data as plain text (issue #51); put the data \
                 in a `Controls.Label` with `textFormat: Text.PlainText` instead:\n{binding}"
            );
        }
    }
    assert!(
        checked >= 3,
        "expected the decision-prompt, Blocklists and Profiles InlineMessages, found {checked}"
    );
}

#[test]
fn fixed_text_matcher_accepts_literals_and_rejects_data() {
    assert!(is_fixed_text("text: \"a\""));
    assert!(is_fixed_text("text: \"a \\\" b\"\n    + \"c\""));
    assert!(!is_fixed_text("text: sheet.process"));
    assert!(!is_fixed_text("text: \"a\" + sheet.host"));
    assert!(!is_fixed_text("text: cond ? \"a\" : \"b\""));
    assert!(!is_fixed_text(""));
}

#[test]
fn text_binding_ignores_nested_children() {
    let block = "\n    type: Warning\n    actions: [\n        Kirigami.Action {\n            \
                 text: \"nested\"\n        }\n    ]\n    text: \"own\"\n        + \"more\"\n    \
                 visible: true\n";
    let binding = text_binding(block).unwrap();
    assert!(
        binding.contains("own") && binding.contains("more"),
        "{binding}"
    );
    assert!(!binding.contains("nested") && !binding.contains("visible"));
}

/// Tooltips: the style's default `ToolTip` text item (reached via the attached
/// `ToolTip.text`) is AutoText under Basic/Fusion, so hovering an elided,
/// markup-named title would render the markup — including remote `<img>`
/// loads. Nothing in the shell may use the attached text; a tooltip is
/// declared explicitly with a PlainText `contentItem`.
#[test]
fn tooltips_render_plain_text_only() {
    for (name, source) in ALL_QML {
        let code = code_lines(source);
        for forbidden in ["ToolTip.text", "ToolTip.toolTip"] {
            assert!(
                !code.contains(forbidden),
                "{name} uses `{forbidden}`, which draws through the style's AutoText tooltip \
                 label (issue #51). Declare `Controls.ToolTip {{ contentItem: Controls.Label {{ \
                 textFormat: Text.PlainText; ... }} }}` instead"
            );
        }
        for block in blocks(&code, "ToolTip {") {
            assert!(
                block.contains("contentItem: Controls.Label {")
                    && block.contains("textFormat: Text.PlainText"),
                "{name} declares a ToolTip without a PlainText Controls.Label contentItem \
                 (issue #51):\n{block}"
            );
        }
    }
}

/// `ALL_QML` must not silently miss a QML file added later.
#[test]
fn all_qml_files_are_covered() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/qml");
    let mut on_disk: Vec<String> = std::fs::read_dir(dir)
        .expect("read qml dir")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".qml"))
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = ALL_QML.iter().map(|(n, _)| n.to_string()).collect();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "ALL_QML in honest_ui_qml_guards.rs is out of sync with qml/"
    );
}

/// Sheet titles are data too (process name, rule name, subscription name,
/// profile name). `Kirigami.OverlaySheet` draws `title` with its own default
/// header `Heading` (AutoText, no `textFormat` hook through `title:`), so
/// `SizedOverlaySheet` supplies a PlainText header once for every sheet — and
/// no page may bypass it with a bare `Kirigami.OverlaySheet`.
#[test]
fn overlay_sheet_titles_are_plain_text() {
    let code = code_lines(SIZED_SHEET);
    let start = code
        .find("header: Kirigami.Heading {")
        .expect("SizedOverlaySheet.qml lost its PlainText header override");
    let header = &code[start..];
    let header = &header[..header.find("\n    }").unwrap_or(header.len())];
    assert!(
        header.contains("textFormat: Text.PlainText") && header.contains("sheet.title"),
        "SizedOverlaySheet.qml's header must render `sheet.title` with \
         `textFormat: Text.PlainText`:\n{header}"
    );
    for (name, source) in [
        ("ConnectionsPage.qml", CONNECTIONS_PAGE),
        ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
        ("ProfilesPage.qml", PROFILES_PAGE),
        ("RecommendedRulesPage.qml", RECOMMENDED_PAGE),
        ("RulesPage.qml", RULES_PAGE),
        ("RuleSimulatorSheet.qml", SIMULATOR_SHEET),
        ("RulesImportSheet.qml", IMPORT_SHEET),
        ("RuleEditorSheet.qml", EDITOR_SHEET),
    ] {
        assert!(
            !code_lines(source).contains("Kirigami.OverlaySheet {"),
            "{name} declares a bare Kirigami.OverlaySheet, bypassing SizedOverlaySheet's \
             PlainText title header (issue #51)"
        );
    }
}

/// The Connections inspector is a side panel, not a SizedOverlaySheet, so the
/// sheet-title guard above no longer covers its title. Replacement for that
/// intent: the panel's heading (the process name is data) must be PlainText.
#[test]
fn inspector_panel_title_is_plain_text() {
    let code = code_lines(include_str!("../qml/InspectorPanel.qml"));
    let start = code
        .find("Kirigami.Heading {")
        .expect("InspectorPanel.qml lost its title heading");
    let heading = &code[start..];
    let heading = &heading[..heading.find("\n        }").unwrap_or(heading.len())];
    assert!(
        heading.contains("textFormat: Text.PlainText") && heading.contains("page.inspectProcess"),
        "InspectorPanel.qml's heading must render the process name as PlainText:\n{heading}"
    );
}

/// `HoverTip` is the inspector panel's tooltip for elided data (process and
/// host names). Like every tooltip it must draw PlainText, never the style's
/// AutoText label.
#[test]
fn hover_tip_draws_plain_text() {
    let code = code_lines(include_str!("../qml/HoverTip.qml"));
    assert!(
        code.contains("textFormat: Text.PlainText") && code.contains("contentItem: Controls.Label"),
        "HoverTip.qml must draw its text through a PlainText contentItem"
    );
}

/// Navigation must go through `StackNav.replaceMain`: a pushed detail page
/// sits on top of the page being left, and a bare `pageStack.replace` swaps
/// that one instead. Only the first-run onboarding Component (its own pushed
/// pages, replaced in place) may call it directly.
#[test]
fn main_navigates_through_the_stack_router() {
    let start = MAIN_QML
        .find("id: silentModePageComponent")
        .expect("main.qml lost the onboarding block");
    let end = MAIN_QML[start..]
        .find("id: curatedGroupsPageComponent")
        .expect("main.qml lost the curated groups component")
        + start;
    let outside = format!("{}{}", &MAIN_QML[..start], &MAIN_QML[end..]);
    assert!(
        !code_lines(&outside).contains("pageStack.replace("),
        "main.qml replaces the page stack's top page directly outside the onboarding block; \
         use nav.replaceMain"
    );
    assert!(
        code_lines(MAIN_QML).contains("nav.replaceMain("),
        "main.qml no longer navigates through StackNav"
    );
}

/// Issue #49: the bridge hardcodes per-connection bytes to 0 and opensnitchd
/// has no per-connection counters, so the "This connection: 0 B sent / 0 B
/// received" readout and the sparkline fed by those same zeros were always
/// false. They stay hidden until a real byte source exists.
#[test]
fn per_connection_traffic_readouts_stay_hidden() {
    let sheet = code_lines(PENDING_SHEET);
    for forbidden in [
        "This connection",
        "bytesSent",
        "bytesReceived",
        "Canvas",
        "seriesJson",
    ] {
        assert!(
            !sheet.contains(forbidden),
            "PendingDecisionSheet.qml shows `{forbidden}` again — the bridge reports zero \
             per-connection bytes, so this would display a fabricated 0 B readout / flat \
             sparkline (issue #49)"
        );
    }
    let page = code_lines(CONNECTIONS_PAGE);
    for forbidden in [
        "inspectBytes",
        "bytesSent:",
        "bytesReceived:",
        "trafficModel",
    ] {
        assert!(
            !page.contains(forbidden),
            "ConnectionsPage.qml feeds `{forbidden}` to the inspector again (issue #49)"
        );
    }
    let main = code_lines(MAIN_QML);
    assert!(
        !main.contains("trafficModelRef"),
        "main.qml threads the zero-fed TrafficModel into ConnectionsPage again (issue #49)"
    );
}
