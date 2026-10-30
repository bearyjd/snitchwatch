//! Learning mode phase L4b: the review page (`qml/LearningReviewPage.qml` and
//! its parts) and its controller.
//!   * Source guards: every label is PlainText (names, paths, hosts and flags
//!     are hostile text), buttons and tick boxes carry fixed text, no verdict
//!     words, nothing starts ticked, learning starts from ONE place, rules are
//!     applied from ONE place (the summary), the limits are stated, the
//!     navigation entry and the banner's Review exist, and the files are
//!     registered.
//!   * A probe in a window (it runs the event loop so the list lays out): the
//!     page over a stand-in controller shows hostile text as text, ticks
//!     nothing, calls the controller only through the explicit buttons, keeps
//!     Apply disabled until every extra confirmation is given; and the REAL
//!     controller refuses what it must (nothing ticked, an unknown choice, an
//!     apply with no summary). The probe never calls `refresh()` or Start on a
//!     real controller: they would read or write the user's files.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines, is_fixed_text, text_binding};

const FILES: [(&str, &str); 6] = [
    (
        "LearningReviewPage.qml",
        include_str!("../qml/LearningReviewPage.qml"),
    ),
    (
        "LearningReviewRow.qml",
        include_str!("../qml/LearningReviewRow.qml"),
    ),
    (
        "LearningStartPanel.qml",
        include_str!("../qml/LearningStartPanel.qml"),
    ),
    (
        "LearningLimitsList.qml",
        include_str!("../qml/LearningLimitsList.qml"),
    ),
    (
        "LearningSummaryDialog.qml",
        include_str!("../qml/LearningSummaryDialog.qml"),
    ),
    (
        "LearningProgressList.qml",
        include_str!("../qml/LearningProgressList.qml"),
    ),
];
const MAIN: &str = include_str!("../qml/main.qml");
const DRAWER: &str = include_str!("../qml/GlobalDrawer.qml");
const BUILD: &str = include_str!("../build.rs");
const PROBE_URL: &str = "qrc:/learning_review_probe.qml";
const DONE_MARKER: &str = "LEARNING_REVIEW_PROBE_DONE";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

/// Words that pass judgement; matched as whole words (so "unverified" is not
/// "verified"). Kept here, not in the QML.
const VERDICT_WORDS: [&str; 24] = [
    "safe",
    "malicious",
    "suspicious",
    "normal",
    "trusted",
    "trustworthy",
    "legitimate",
    "benign",
    "harmless",
    "dangerous",
    "malware",
    "virus",
    "verified",
    "genuine",
    "bad",
    "good",
    "clean",
    "infected",
    "attack",
    "attacker",
    "unsafe",
    "suspect",
    "hostile",
    "threat",
];

/// Every button block: plain ones and the held-key-guarded ones.
fn buttons(code: &str) -> Vec<String> {
    let mut found = blocks(code, "Controls.Button {");
    found.extend(blocks(code, "GuardedButton {"));
    found
}

fn source(name: &str) -> &'static str {
    FILES.iter().find(|(n, _)| *n == name).expect("file").1
}

fn all_code() -> String {
    FILES
        .iter()
        .map(|(_, s)| code_lines(s))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The double-quoted string literals of QML code.
fn literals(code: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = code.chars();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut text = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        text.push(next);
                    }
                }
                '"' => break,
                other => text.push(other),
            }
        }
        found.push(text);
    }
    found
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .collect()
}

#[test]
fn every_label_in_the_review_files_is_plain_text() {
    for (name, src) in FILES {
        let code = code_lines(src);
        for open in ["Controls.Label {", "Kirigami.Heading {", "Text {"] {
            for block in blocks(&code, open) {
                assert!(
                    block.contains("textFormat: Text.PlainText"),
                    "{name}: a `{open}` can render markup (issue #51):\n{block}"
                );
            }
        }
        for banned in [
            "Text.StyledText",
            "Text.RichText",
            "Text.AutoText",
            "Text.MarkdownText",
            "Kirigami.InlineMessage",
            "Kirigami.PlaceholderMessage",
            "ToolTip.text",
        ] {
            assert!(!code.contains(banned), "{name}: {banned}");
        }
    }
}

#[test]
fn buttons_carry_fixed_text_and_tick_boxes_none() {
    for (name, src) in FILES {
        let code = code_lines(src);
        for block in buttons(&code) {
            if let Some(text) = text_binding(&block) {
                assert!(is_fixed_text(&text), "{name}: a button shows data: {text}");
            }
        }
        for block in blocks(&code, "Controls.CheckBox {") {
            assert!(
                text_binding(&block).is_none(),
                "{name}: a tick box text would render as markup:\n{block}"
            );
        }
    }
}

#[test]
fn no_string_in_the_review_files_is_a_verdict() {
    for (name, src) in FILES {
        for literal in literals(&code_lines(src)) {
            for word in words(&literal) {
                assert!(
                    !VERDICT_WORDS.contains(&word.as_str()),
                    "{name}: {literal:?} says {word:?}"
                );
            }
        }
    }
}

#[test]
fn the_verdict_guard_can_fail() {
    let flagged = literals(r#"text: "a good program" + "an unverified one""#);
    let hits: Vec<String> = flagged
        .iter()
        .flat_map(|l| words(l))
        .filter(|w| VERDICT_WORDS.contains(&w.as_str()))
        .collect();
    assert_eq!(hits, vec!["good".to_string()]);
}

#[test]
fn nothing_is_ticked_by_default_and_only_the_owner_ticks() {
    let code = all_code();
    assert!(!code.contains("checked: true"), "a box starts ticked");
    let row = code_lines(source("LearningReviewRow.qml"));
    assert!(row.contains("checked: row.selected === modelData.kind"));
    // The only way to a tick is the owner's click or the explicit button.
    assert_eq!(row.matches("setTick(").count(), 1);
    let page = code_lines(source("LearningReviewPage.qml"));
    assert_eq!(page.matches("selectSuggested()").count(), 1);
    let suggest = blocks(&page, "Controls.Button {")
        .into_iter()
        .find(|b| b.contains("selectSuggested()"))
        .expect("the button");
    assert!(suggest.contains("text: \"Select suggested\""));
    assert!(suggest.contains("onClicked:"), "{suggest}");
    for line in all_code()
        .lines()
        .filter(|l| l.contains("Component.onCompleted"))
    {
        assert!(
            !line.contains("setTick(") && !line.contains("selectSuggested()"),
            "something ticks on load: {line}"
        );
    }
}

#[test]
fn learning_starts_from_one_place_and_rules_are_applied_from_one_place() {
    let code = all_code();
    assert_eq!(code.matches("startLearning(").count(), 1, "{code}");
    let start = code_lines(source("LearningStartPanel.qml"));
    assert!(start.contains("panel.settings.startLearning(panel.chosenHours)"));
    // 24 hours by default, 7 days the most.
    assert!(start.contains("currentIndex: 2"));
    assert!(start.contains("[1, 6, 24, 72, 168]"));
    assert!(start.contains("\"24 hours (default)\"") && start.contains("\"7 days (the longest)\""));

    assert_eq!(
        code.matches(".apply()").count(),
        1,
        "apply is called from more than one place"
    );
    let dialog = code_lines(source("LearningSummaryDialog.qml"));
    let apply = buttons(&dialog)
        .into_iter()
        .find(|b| b.contains("learningApplyButton"))
        .expect("the Apply button");
    assert!(apply.contains("enabled: dialog.allConfirmed"), "{apply}");
    assert!(apply.contains("dialog.controller.apply()"));
    assert!(apply.contains("\"Apply \""), "{apply}");
}

#[test]
fn the_page_states_the_limits_of_learning() {
    let limits = source("LearningLimitsList.qml");
    for needle in [
        "allowed once",
        "Blocklists and deny rules still apply",
        "only while Snitchwatch is open",
        "at most 8 programs",
        "cut short",
        "does not show what a program is",
        "known limit",
        "is never ticked by",
        "yourself needs an extra confirmation",
        "denied by Snitchwatch's default configuration",
    ] {
        assert!(
            limits.contains(needle),
            "the limits no longer say: {needle}"
        );
    }
    let page = code_lines(source("LearningReviewPage.qml"));
    assert!(page.contains("LearningLimitsList {"));
    assert!(page.contains("LearningStartPanel {"));
    assert!(page.contains("LearningSummaryDialog {"));
    assert!(page.contains("page.controller.discardSummary();\n        page.controller.refresh();"));
    assert!(page.contains(
        "Component.onDestruction: if (page.controller) page.controller.discardSummary()"
    ));
}

#[test]
fn the_page_is_reachable_registered_and_main_qml_stays_small() {
    let code = code_lines(MAIN);
    assert_eq!(blocks(&code, "LearningReviewController {").len(), 1);
    let page = blocks(&code, "LearningReviewPage {");
    assert_eq!(page.len(), 1);
    assert!(page[0].contains("controller: learningReviewController"));
    assert!(page[0].contains("settings: settingsController"));
    // The drawer lives in GlobalDrawer.qml; main.qml registers the page.
    let entry = blocks(&code_lines(DRAWER), "Kirigami.Action {")
        .into_iter()
        .find(|b| b.contains("Make rules from what I saw"))
        .expect("no navigation entry");
    assert!(entry.contains("drawer.openPage(\"learningReview\")"));
    assert!(
        code.contains("learningReview: learningReviewPageComponent"),
        "main.qml does not register the review page for the drawer"
    );
    assert!(
        MAIN.lines().count() <= 800,
        "main.qml is {} lines",
        MAIN.lines().count()
    );
    for (name, _) in FILES {
        assert!(
            BUILD.contains(&format!("\"qml/{name}\"")),
            "{name} is not in build.rs"
        );
    }
    assert!(BUILD.contains(".file(\"src/learning_review_controller.rs\")"));
}

#[test]
fn the_review_files_stay_small() {
    for (name, src) in FILES {
        assert!(
            src.lines().count() <= 400,
            "{name} is {} lines",
            src.lines().count()
        );
    }
}

#[test]
fn the_page_and_the_real_controller_behave() {
    init_headless_qt_env();
    // Nothing here may reach the user's own files.
    let home = tempfile::tempdir().expect("temp config home");
    std::env::set_var("XDG_CONFIG_HOME", home.path());

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(PROBE), &QUrl::from(PROBE_URL));
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "learning review probe failed to load (QML parse error); stderr:\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| !line.contains(DONE_MARKER))
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "learning review probe reported errors:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "the learning review probe never finished; stderr:\n{captured}"
    );
}

const PROBE: &str = include_str!("probes/learning_review_probe.qml");

#[test]
fn the_limits_are_eight_lines_each_scanned_and_the_button_rows_wrap() {
    let limits = code_lines(source("LearningLimitsList.qml"));
    let long: Vec<String> = literals(&limits)
        .into_iter()
        .filter(|l| l.len() > 60)
        .collect();
    assert_eq!(long.len(), 8, "{long:?}");
    for line in &long {
        for word in words(line) {
            assert!(!VERDICT_WORDS.contains(&word.as_str()), "{line:?}: {word}");
        }
    }
    assert!(limits.contains("a bridge restart (the in-memory sets are lost)"));
    assert!(limits.contains("and to one compromised during the learning window"));
    // Rows of buttons wrap instead of clipping.
    for name in ["LearningReviewPage.qml", "LearningStartPanel.qml"] {
        let code = code_lines(source(name));
        for row in blocks(&code, "RowLayout {") {
            assert!(
                !row.contains("Controls.Button {"),
                "{name}: a RowLayout of buttons clips"
            );
        }
        assert!(code.contains("Flow {"), "{name}");
    }
}

#[test]
fn many_rules_are_listed_in_list_views_not_repeaters() {
    for name in ["LearningSummaryDialog.qml", "LearningProgressList.qml"] {
        let code = code_lines(source(name));
        assert!(
            !code.contains("Repeater {"),
            "{name} instantiates every rule"
        );
        assert!(code.contains("ListView {"), "{name}");
    }
}

#[test]
fn deleting_the_record_asks_first() {
    let page = code_lines(source("LearningReviewPage.qml"));
    assert_eq!(page.matches("clearData()").count(), 1);
    let yes = buttons(&page)
        .into_iter()
        .find(|b| b.contains("learningDeleteYes"))
        .expect("the confirming button");
    assert!(yes.contains("page.controller.clearData()"));
    let first = buttons(&page)
        .into_iter()
        .find(|b| b.contains("learningClearButton"))
        .unwrap();
    assert!(first.contains("page.confirmingDelete = true") && !first.contains("clearData"));
}
