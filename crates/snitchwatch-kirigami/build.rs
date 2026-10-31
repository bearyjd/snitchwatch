//! cxx-qt build script for the Snitchwatch Kirigami shell.
//!
//! Drives moc/rcc/qmlcachegen and the cxx-qt C++/QML-registration codegen.
//! The QML module is registered under the canonical `com.snitchwatch.shell`
//! URI; `main.rs` loads its entry point from the generated `qrc:/` path.
//!
//! Rust `#[cxx_qt::bridge]` modules that back QML types are added via
//! `.file(...)` so their generated code is compiled and statically registered.
use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

#[path = "build_commit.rs"]
mod build_commit;
use build_commit::{is_commit_id, is_directive_safe_path};

fn main() {
    build_identity();
    let mut builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("com.snitchwatch.shell")
            // `Theme.qml` is a `pragma Singleton`; it needs the qmldir
            // `singleton` flag, so it is declared as a `QmlFile`.
            .qml_file(QmlFile::from("qml/Theme.qml").singleton(true))
            .qml_file(QmlFile::from("qml/Durations.qml").singleton(true))
            .qml_file(QmlFile::from("qml/SilentModes.qml").singleton(true))
            .qml_file(QmlFile::from("qml/MapBandState.qml").singleton(true))
            .qml_file(QmlFile::from("qml/SelectionRegistry.qml").singleton(true))
            .qml_files([
                "qml/main.qml",
                "qml/TrayMenu.qml",
                "qml/SizedOverlaySheet.qml",
                "qml/ConnectionsPage.qml",
                "qml/ConnectionRow.qml",
                "qml/PendingAgeTicker.qml",
                "qml/ConnectionActions.qml",
                "qml/ClipboardCopy.qml",
                "qml/ConnectionDetailsCopy.qml",
                "qml/ConnectionExplanation.qml",
                "qml/RuleScopePreview.qml",
                "qml/ConnectionsHeader.qml",
                "qml/ConnectionsFilterBar.qml",
                "qml/ConnectionsTopBar.qml",
                "qml/AnalysisExportSheet.qml",
                "qml/StackNav.qml",
                "qml/HoverTip.qml",
                "qml/ProgramLabel.qml",
                "qml/GroupVerdictCounts.qml",
                "qml/GroupPendingBadge.qml",
                "qml/BackgroundGroupBar.qml",
                "qml/InspectorState.qml",
                "qml/InspectorPanel.qml",
                "qml/ConnectionDetailPage.qml",
                "qml/ConnectionList.qml",
                "qml/MapBand.qml",
                "qml/MapView.qml",
                "qml/MapHierarchy.qml",
                "qml/MapDrilldown.qml",
                "qml/MapFocus.qml",
                "qml/MapDestinationsPopup.qml",
                "qml/MapCamera.qml",
                "qml/MapNavigation.qml",
                "qml/MapMarker.qml",
                "qml/MonitorPane.qml",
                "qml/RuleButton.qml",
                "qml/LevelRuleActions.qml",
                "qml/InlineVerdicts.qml",
                "qml/PromptSlotBanner.qml",
                "qml/GuardedButton.qml",
                "qml/GuardedToolButton.qml",
                "qml/GuardedSwitch.qml",
                "qml/GuardedRadioButton.qml",
                "qml/NavigableRow.qml",
                "qml/FocusOrderListView.qml",
                "qml/LearningBanner.qml",
                "qml/LearningReviewPage.qml",
                "qml/LearningReviewRow.qml",
                "qml/LearningStartPanel.qml",
                "qml/LearningLimitsList.qml",
                "qml/LearningSummaryDialog.qml",
                "qml/LearningProgressList.qml",
                "qml/PendingDecisionSheet.qml",
                "qml/DecideLaterButton.qml",
                "qml/MakeRuleSheet.qml",
                "qml/MakeRuleOutcomes.qml",
                "qml/BlocklistsPage.qml",
                "qml/RulesPage.qml",
                "qml/RulesInspectorSheet.qml",
                "qml/RulesInsightsText.qml",
                "qml/RulesEmptyPlaceholder.qml",
                "qml/RuleSimulatorSheet.qml",
                "qml/RulesImportSheet.qml",
                "qml/RuleEditorSheet.qml",
                "qml/ProfilesPage.qml",
                "qml/RecommendedRulesPage.qml",
                "qml/DnsConsentDialog.qml",
                "qml/CuratedGroupsStep.qml",
                "qml/CuratedGroupsPage.qml",
                "qml/TrafficPage.qml",
                "qml/OnboardingPage.qml",
                "qml/SilentModeChoice.qml",
                "qml/SilentModePage.qml",
                "qml/AlertHost.qml",
                "qml/ScreenshotRunner.qml",
                "qml/SegmentedChoice.qml",
                "qml/AlertWindow.qml",
                "qml/DiagnosticsPage.qml",
                "qml/GeoPage.qml",
                "qml/GeoDatabaseSettings.qml",
                "qml/ScannerPage.qml",
                "qml/DaemonHealthPage.qml",
                "qml/DenyStatusSection.qml",
                "qml/AboutPage.qml",
                "qml/GlobalDrawer.qml",
                "qml/SelectableText.qml",
                "qml/RowCopyMenu.qml",
                "qml/GuardedMenuItem.qml",
            ]),
    )
    .qt_module("Widgets")
    .file("src/application.rs")
    .file("src/clipboard.rs")
    // The native read accessor is linked only by its regression test and is
    // never exposed to the application's QML module.
    .file("src/clipboard_native_test.rs")
    .file("src/delegate_incubation_test.rs")
    .file("src/accessibility_test.rs")
    .file("src/bridge_bindings.rs")
    .file("src/bridge_feed.rs")
    .file("src/connections_model.rs")
    .file("src/insight_model.rs")
    .file("src/blocklists_model.rs")
    .file("src/rules_model.rs")
    .file("src/rules_io_controller.rs")
    .file("src/analysis_export_controller.rs")
    // Native factory/inspection wrappers are referenced only by cfg(test)
    // controller tests; no QML or production injection API is registered.
    .file("src/analysis_export_native_test.rs")
    .file("src/rule_editor_controller.rs")
    .file("src/make_rule_controller.rs")
    .file("src/profiles_model.rs")
    .file("src/curated_defaults_model.rs")
    .file("src/traffic_model.rs")
    .file("src/wizard_controller.rs")
    .file("src/settings_controller.rs")
    .file("src/notification_controller.rs")
    .file("src/tray_controller.rs")
    .file("src/prompt_slot_status.rs")
    .file("src/geo_model.rs")
    .file("src/scanner_controller.rs")
    .file("src/learning_review_controller.rs")
    .file("src/daemon_health_model.rs")
    .file("src/deny_status_model.rs")
    .file("src/about_model.rs");
    // The auto-repeat key filter exists only for tests: it is compiled and
    // linked solely under the `test-seams` feature (the self dev-dependency),
    // never into a shipped build. Guarded by tests/test_seams_gate_guard.rs.
    if std::env::var_os("CARGO_FEATURE_TEST_SEAMS").is_some() {
        builder = builder.file("src/key_repeat_test.rs");
    }
    // SAFETY: only add the crate-owned header directory; preserve builder flags.
    unsafe {
        builder.cc_builder(|cc| {
            cc.include("include");
            cc.include("tests/support");
        })
    }
    .build();
    println!("cargo:rerun-if-changed=include/application.h");
    println!("cargo:rerun-if-changed=include/clipboard.h");
    println!("cargo:rerun-if-changed=include/qt_runtime.h");
    println!("cargo:rerun-if-changed=tests/support/clipboard_native.h");
    println!("cargo:rerun-if-changed=tests/support/delegate_incubation.h");
    println!("cargo:rerun-if-changed=tests/support/accessibility.h");
    println!("cargo:rerun-if-changed=tests/support/key_repeat.h");
}

/// Records which source this binary was built from, as `rustc-env` constants
/// for `src/about.rs`. Never fails the build: a source tarball (the Flatpak
/// build) has no `.git`, so the commit then comes from
/// `SNITCHWATCH_BUILD_COMMIT`, else is left empty ("not recorded").
fn build_identity() {
    use std::path::PathBuf;
    use std::process::Command;

    println!("cargo:rerun-if-env-changed=SNITCHWATCH_BUILD_COMMIT");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    // Only trust git when the checkout root is ours; a tarball unpacked inside
    // some unrelated repository must not record that repository's commit.
    let root = manifest.join("../..");
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let mut commit = String::new();
    let mut dirty = false;
    if root.join(".git").exists() {
        if let Some(head) = git(&["rev-parse", "HEAD"]).filter(|c| is_commit_id(c)) {
            commit = head;
            // Submodule noise (an untracked tooling dir) is not a change here.
            dirty = git(&[
                "status",
                "--porcelain",
                "--untracked-files=no",
                "--ignore-submodules",
            ])
            .is_some_and(|s| !s.is_empty());
            // Re-run when HEAD or the branch it names moves. A dirty flag can
            // go stale until the next rebuild; that is accepted.
            if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", "HEAD"])
                .filter(|p| is_directive_safe_path(p))
            {
                println!("cargo:rerun-if-changed={path}");
            }
            if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
                if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &r])
                    .filter(|p| is_directive_safe_path(p))
                {
                    println!("cargo:rerun-if-changed={path}");
                }
            }
        }
    }
    if commit.is_empty() {
        if let Ok(env) = std::env::var("SNITCHWATCH_BUILD_COMMIT") {
            let env = env.trim();
            if is_commit_id(env) {
                commit = env.to_string();
            }
        }
    }
    println!("cargo:rustc-env=SNITCHWATCH_BUILD_COMMIT_RESOLVED={commit}");
    println!(
        "cargo:rustc-env=SNITCHWATCH_BUILD_DIRTY={}",
        u8::from(dirty)
    );
}
