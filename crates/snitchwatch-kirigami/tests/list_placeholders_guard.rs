//! Source guards (PR #106's lesson, issue #114): a `ScrollablePage` hides
//! every child but its list, so an empty-list placeholder beside the list
//! never shows. Small empty-list notices sit on the list (`parent: list`),
//! as `RulesEmptyPlaceholder` does. Geo's taller database setup belongs in
//! the scrollable header so every control and feedback message is reachable.
//! Crude text assertions, for the reason given in `honest_ui_qml_guards.rs`.

mod qml_guard_support;
use qml_guard_support::{blocks, code_lines, has_line};

const LIST_OPEN: &str = "ListView {\n        id: list";

fn assert_placeholders_sit_on_the_list(page: &str, source: &str) {
    let code = code_lines(source);
    let list = code
        .find(LIST_OPEN)
        .unwrap_or_else(|| panic!("{page}: no `ListView {{ id: list`"));
    assert!(
        !code[..list].contains("PlaceholderMessage {"),
        "{page}: a placeholder beside the list is hidden by the ScrollablePage"
    );
    let on_list = &code[list..];
    assert!(
        on_list.contains("PlaceholderMessage {\n            parent: list\n"),
        "{page}: the placeholder must be a child of the list with `parent: list`"
    );
}

#[test]
fn blocklists_geo_and_profiles_placeholders_sit_on_their_list() {
    assert_placeholders_sit_on_the_list(
        "BlocklistsPage.qml",
        include_str!("../qml/BlocklistsPage.qml"),
    );
    assert_placeholders_sit_on_the_list("GeoPage.qml", include_str!("../qml/GeoPage.qml"));
    assert_placeholders_sit_on_the_list(
        "ProfilesPage.qml",
        include_str!("../qml/ProfilesPage.qml"),
    );
}

/// Geo's other empty state includes controls that exceed a small viewport.
/// The actual 500x320 scrolling behavior is also exercised by geo_qml.rs.
#[test]
fn geos_no_database_setup_is_in_the_scrollable_list_header() {
    let code = code_lines(include_str!("../qml/GeoPage.qml"));
    let list = blocks(&code, "ListView {")
        .into_iter()
        .find(|list| has_line(list, "id: list"))
        .expect("Geo must own its empty states inside the page's ListView");
    let header = blocks(&list, "header: ColumnLayout {")
        .into_iter()
        .next()
        .expect("Geo setup must contribute to the list's scrollable content height");
    assert!(has_line(&header, "width: list.width"));
    let setup = blocks(&header, "ColumnLayout {")
        .into_iter()
        .find(|setup| has_line(setup, "visible: page.model && !page.model.dbAvailable"))
        .expect("The no-database setup must be inside the scrollable header");
    assert!(
        !setup.contains("anchors.centerIn:"),
        "Setup must flow in the header"
    );
    assert!(
        !setup.contains("parent: list"),
        "Setup must not become a list overlay"
    );
    assert!(blocks(&setup, "Kirigami.PlaceholderMessage {")
        .iter()
        .any(|notice| { has_line(notice, "text: \"No GeoIP database installed\"") }));
    assert!(
        blocks(&setup, "GeoDatabaseSettings {")
            .iter()
            .any(|settings| {
                has_line(settings, "Layout.fillWidth: true")
                    && has_line(settings, "model: page.model")
            }),
        "The setup controls and feedback must scroll with the no-database notice"
    );
}

/// The window-level controller lives in `main.qml`, not in a page or sheet a
/// drawer switch destroys (PR #111 review, OQ1).
#[test]
fn the_make_rule_controller_lives_in_main_qml_only() {
    let main = code_lines(include_str!("../qml/main.qml"));
    assert!(main.contains(
        "MakeRuleController {\n        id: makeRuleController\n        Component.onCompleted: startBridgeFeed()"
    ));
    for (name, source) in [
        (
            "RulesPage.qml",
            concat!(
                include_str!("../qml/RulesPage.qml"),
                "\n",
                include_str!("../qml/RulesInspectorSheet.qml")
            ),
        ),
        (
            "MakeRuleSheet.qml",
            include_str!("../qml/MakeRuleSheet.qml"),
        ),
        (
            "ConnectionsPage.qml",
            include_str!("../qml/ConnectionsPage.qml"),
        ),
    ] {
        assert!(
            !code_lines(source).contains("MakeRuleController {"),
            "{name} must not own a MakeRuleController"
        );
    }
}

/// An ending the notice doesn't know says "unknown", never "couldn't be
/// created": only an ending of 0 is a confirmed refusal.
#[test]
fn an_unexpected_ending_reads_as_unknown_not_as_refused() {
    let code = code_lines(include_str!("../qml/MakeRuleOutcomes.qml"));
    assert!(code.contains(
        "ending === 1 ? outcomes.createdText : ending === 0 ? outcomes.notCreatedText : outcomes.unknownText"
    ));
}
