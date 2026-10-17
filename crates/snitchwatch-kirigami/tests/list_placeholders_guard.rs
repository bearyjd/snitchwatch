//! Source guards (PR #106's lesson, issue #114): a `ScrollablePage` hides
//! every child but its list, so an empty-list placeholder beside the list
//! never shows. On the Blocklists, Geo and Profiles pages it sits on the list
//! (`parent: list`), as `RulesEmptyPlaceholder` does. Crude text assertions,
//! for the reason given in `honest_ui_qml_guards.rs`.

mod qml_guard_support;
use qml_guard_support::code_lines;

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

/// Geo's "no database" notice is the page's other empty state.
#[test]
fn geos_no_database_notice_sits_on_the_list_too() {
    let code = code_lines(include_str!("../qml/GeoPage.qml"));
    let list = code.find(LIST_OPEN).unwrap();
    assert!(!code[..list].contains("ColumnLayout {"));
    assert!(code[list..].contains("ColumnLayout {\n            parent: list\n"));
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
