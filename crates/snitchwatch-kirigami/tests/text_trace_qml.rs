//! `SNITCHWATCH_TEXT_TRACE` guards (docs/superpowers/plans/2026-11-03-text-trace.md):
//! main.qml mounts the tracer only behind `appInfo.textTrace`, so with the
//! flag off nothing is instantiated; the file is registered in build.rs; and
//! main.qml stays under the 800-line cap.
mod qml_guard_support;

use qml_guard_support::{blocks, code_lines};

const MAIN: &str = include_str!("../qml/main.qml");
const BUILD: &str = include_str!("../build.rs");
const TRACE: &str = include_str!("../qml/TextTrace.qml");

#[test]
fn main_qml_mounts_the_tracer_only_behind_the_flag() {
    let code = code_lines(MAIN);
    assert_eq!(code.matches("TextTrace {").count(), 1, "one tracer mount");
    let loaders = blocks(&code, "Loader {");
    let mount: Vec<_> = loaders
        .iter()
        .filter(|b| b.contains("TextTrace {"))
        .collect();
    assert_eq!(mount.len(), 1, "the tracer must sit in a Loader");
    assert!(
        mount[0].contains("active: appInfo.textTrace"),
        "the Loader is not gated on appInfo.textTrace"
    );
    assert!(
        mount[0].contains("trayController.tooltip"),
        "tray tooltip not traced"
    );
    assert!(
        MAIN.lines().count() <= 800,
        "main.qml is {} lines",
        MAIN.lines().count()
    );
}

#[test]
fn tracer_is_registered_and_never_logs_without_being_asked() {
    assert!(
        BUILD.contains("\"qml/TextTrace.qml\""),
        "not registered in build.rs"
    );
    // Change-driven: no polling timer.
    assert!(
        !code_lines(TRACE).contains("Timer {"),
        "tracer must not poll"
    );
}

/// `textTraceTitle` makes the tracer log the page's own `title`: it is only
/// allowed on a page whose title is a string literal in that page's QML.
#[test]
fn only_literal_title_pages_declare_text_trace_title() {
    let pages: [(&str, &str); 15] = [
        ("AboutPage", include_str!("../qml/AboutPage.qml")),
        ("OnboardingPage", include_str!("../qml/OnboardingPage.qml")),
        ("BlocklistsPage", include_str!("../qml/BlocklistsPage.qml")),
        (
            "CuratedGroupsPage",
            include_str!("../qml/CuratedGroupsPage.qml"),
        ),
        (
            "DaemonHealthPage",
            include_str!("../qml/DaemonHealthPage.qml"),
        ),
        ("RulesPage", include_str!("../qml/RulesPage.qml")),
        ("ScannerPage", include_str!("../qml/ScannerPage.qml")),
        (
            "LearningReviewPage",
            include_str!("../qml/LearningReviewPage.qml"),
        ),
        ("ProfilesPage", include_str!("../qml/ProfilesPage.qml")),
        (
            "DiagnosticsPage",
            include_str!("../qml/DiagnosticsPage.qml"),
        ),
        (
            "RecommendedRulesPage",
            include_str!("../qml/RecommendedRulesPage.qml"),
        ),
        (
            "ConnectionsPage",
            include_str!("../qml/ConnectionsPage.qml"),
        ),
        ("GeoPage", include_str!("../qml/GeoPage.qml")),
        ("TrafficPage", include_str!("../qml/TrafficPage.qml")),
        ("SilentModePage", include_str!("../qml/SilentModePage.qml")),
    ];
    for (name, src) in pages {
        let title = code_lines(src)
            .lines()
            .find(|l| l.trim_start().starts_with("title:"))
            .unwrap_or_else(|| panic!("{name}: no title line"))
            .trim()
            .to_string();
        assert!(
            title.starts_with("title: \""),
            "{name}: not literal: {title}"
        );
        assert!(src.contains("property bool textTraceTitle: true"), "{name}");
    }
    // Its title is the inspected process name: dynamic, so never marked.
    let detail = include_str!("../qml/ConnectionDetailPage.qml");
    assert!(
        !detail.contains("textTraceTitle"),
        "ConnectionDetailPage must stay unmarked"
    );
}
