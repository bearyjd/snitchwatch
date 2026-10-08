//! Source-level honest-UI guards for the Profiles page (issues #46, #51):
//! it says whether profiles are applied to the firewall, only as the
//! Snitchwatch service reports it, and every label showing profile or
//! firewall data is plain text. Same crude text checks as
//! `honest_ui_qml_guards.rs`, for the same reasons.

mod qml_guard_support;
use qml_guard_support::*;

const PROFILES_PAGE: &str = include_str!("../qml/ProfilesPage.qml");

/// The header's fixed-text messages:
/// - keyed on `page.appliesRules` (an Information note): profiles are
///   applied, and each rule shows whether the firewall accepted it;
/// - keyed on `!page.appliesRules` (a Warning): profiles are not applied
///   here, activating one installs no firewall rules; with the reason in a
///   PlainText label;
/// - keyed on `!page.storagePersistent` (a Warning): lost on restart.
///
/// None is dismissable, none says "bridge" or "Preview", and none carries
/// data.
#[test]
fn the_profiles_page_says_whether_profiles_are_applied() {
    let code = code_lines(PROFILES_PAGE);
    for line in [
        "readonly property bool storagePersistent: page.model ? page.model.storagePersistent : false",
        "readonly property string storageReason: page.model ? page.model.storageReason : \"\"",
        "readonly property bool appliesRules: page.model ? page.model.appliesRules : false",
        "readonly property string notAppliedReason: page.model ? page.model.notAppliedReason : \"\"",
    ] {
        assert!(has_line(&code, line), "ProfilesPage.qml must declare `{line}`");
    }
    let header = blocks(&code, "header: ColumnLayout {");
    assert_eq!(header.len(), 1, "ProfilesPage.qml lost its banner header");
    let banners = blocks(&header[0], "Kirigami.InlineMessage {");
    assert_eq!(banners.len(), 3, "expected three messages");
    banners
        .iter()
        .for_each(|banner| assert_fixed_and_undismissable(banner));
    assert_keyed(
        &banners,
        "page.appliesRules",
        "Information",
        &[
            "applied to the firewall",
            "whether the firewall accepted it",
        ],
    );
    assert_keyed(
        &banners,
        "!page.appliesRules",
        "Warning",
        &["not applied to the firewall", "no firewall rules"],
    );
    assert_keyed(
        &banners,
        "!page.storagePersistent",
        "Warning",
        &["memory only", "restart"],
    );
    assert_header_labels(&blocks(&header[0], "Controls.Label {"));
}

fn assert_fixed_and_undismissable(banner: &str) {
    assert!(
        !banner.contains("showCloseButton: true") && !banner.contains("actions:"),
        "a profiles message must not be dismissable:\n{banner}"
    );
    assert!(!banner.contains("bridge"), "internal jargon:\n{banner}");
    assert!(
        !banner.contains("Preview"),
        "profiles are no preview:\n{banner}"
    );
    assert!(
        is_fixed_text(&text_binding(banner).unwrap_or_default()),
        "a message carries data:\n{banner}"
    );
}

/// The message shown for `visible` is of `kind` and says each phrase.
fn assert_keyed(banners: &[String], visible: &str, kind: &str, says: &[&str]) {
    let banner = banners
        .iter()
        .find(|b| has_line(b, &format!("visible: {visible}")))
        .unwrap_or_else(|| panic!("no message keyed on `{visible}`"));
    assert!(
        banner.contains(&format!("type: Kirigami.MessageType.{kind}")),
        "the `{visible}` message must be {kind}:\n{banner}"
    );
    for phrase in says {
        assert!(
            banner.contains(phrase),
            "the `{visible}` message must say \"{phrase}\":\n{banner}"
        );
    }
}

/// The header's labels: a fixed-text note shown while profiles are saved,
/// and the storage and not-applied reasons each in one PlainText label.
fn assert_header_labels(labels: &[String]) {
    let saved = labels
        .iter()
        .find(|l| has_line(l, "visible: page.storagePersistent"))
        .expect("no note saying profiles are saved");
    assert!(
        saved.contains("are saved") && is_fixed_text(&text_binding(saved).unwrap_or_default()),
        "the saved note must be fixed text saying profiles are saved:\n{saved}"
    );
    for reason in ["page.storageReason", "page.notAppliedReason"] {
        let shown: Vec<_> = labels.iter().filter(|l| l.contains(reason)).collect();
        assert_eq!(shown.len(), 1, "expected one label for {reason}");
        assert!(
            shown[0].contains("textFormat: Text.PlainText"),
            "{reason} must be a PlainText label:\n{}",
            shown[0]
        );
    }
}

#[test]
fn profiles_page_labels_showing_profile_data_are_plain_text() {
    assert_data_labels_plain_text(
        "ProfilesPage.qml",
        PROFILES_PAGE,
        &[
            "row.name",
            "row.networkMatchers",
            "page.storageReason",
            "page.notAppliedReason",
            // Issue #46 Part 2: rules, their status and the firewall's text.
            "ruleRow.modelData.action",
            "ruleRow.modelData.conditions",
            "ruleRow.modelData.status",
            "ruleRow.modelData.reason",
            "ruleEditorController.statusText",
        ],
        9,
    );
}
