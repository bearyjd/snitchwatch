//! Learning mode phase L2: the banner (`qml/LearningBanner.qml`).
//!   * Source guards: its sentence is fixed text, every label is PlainText,
//!     it has a Stop and a Review button (L4b added Review, and with it the
//!     review page), and `main.qml` mounts it, opens the review page from it
//!     and stays under the 800-line cap.
//!   * A probe in the window: hidden without a window; with one it shows the
//!     time left, the count (singular and plural), the incomplete and dropped
//!     notices, the file notice and path as plain text (markup shows as text),
//!     and "Stop learning" calls `stopLearning()` once.
//!
//! Like `honest_ui_main_banner_qml.rs`, the probe never runs the event loop:
//! creating the banner evaluates its bindings synchronously, and the probe
//! throws on any mismatch. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines, is_fixed_text, text_binding};

const BANNER: &str = include_str!("../qml/LearningBanner.qml");
const MAIN: &str = include_str!("../qml/main.qml");
const BUILD: &str = include_str!("../build.rs");
const CONTROLLER: &str = include_str!("../src/settings_controller.rs");
const PROBE_URL: &str = "qrc:/learning_banner_probe.qml";
const DONE_MARKER: &str = "LEARNING_BANNER_PROBE_DONE";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn the_message_is_fixed_text_and_every_dynamic_label_is_plain_text() {
    let code = code_lines(BANNER);
    let messages = blocks(&code, "Kirigami.InlineMessage {");
    assert_eq!(messages.len(), 1, "one InlineMessage, with fixed text");
    let sentence = text_binding(&messages[0]).expect("the message has a text");
    assert!(
        is_fixed_text(&sentence),
        "the message carries data: {sentence}"
    );
    assert!(
        sentence.contains("Learning: new connections are allowed once and recorded."),
        "{sentence}"
    );

    let labels = blocks(&code, "Controls.Label {");
    assert_eq!(labels.len(), 5, "the labels moved: {}", labels.len());
    for label in &labels {
        assert!(
            label.contains("textFormat: Text.PlainText"),
            "a banner label can render markup (issue #51):\n{label}"
        );
    }
    // Text that varies is only ever in those labels.
    for banned in [
        "Text.StyledText",
        "Text.RichText",
        "Text.AutoText",
        "Text.MarkdownText",
    ] {
        assert!(!code.contains(banned), "{banned}");
    }
    assert!(
        labels
            .iter()
            .any(|l| text_binding(l).is_some_and(|b| b.contains("learnedNotice"))),
        "the file notice has no label"
    );
}

#[test]
fn stop_and_review_are_the_only_actions() {
    let code = code_lines(BANNER);
    assert_eq!(code.matches("Kirigami.Action {").count(), 2);
    assert!(code.contains("text: \"Stop learning\""));
    assert!(code.contains("banner.settings.stopLearning()"));
    assert!(code.contains("text: \"Review\""));
    assert!(code.contains("signal reviewRequested()"));
    assert!(code.contains("banner.reviewRequested()"));
}

#[test]
fn main_qml_mounts_the_banner_and_stays_under_the_line_cap() {
    let code = code_lines(MAIN);
    let mounts = blocks(&code, "LearningBanner {");
    assert_eq!(mounts.len(), 1);
    assert!(mounts[0].contains("settings: settingsController"));
    assert!(
        mounts[0].contains("onReviewRequested: nav.replaceMain(learningReviewPageComponent)"),
        "the banner's Review does not open the review page"
    );
    assert!(
        MAIN.lines().count() <= 800,
        "main.qml is {} lines",
        MAIN.lines().count()
    );
    assert!(
        BUILD.contains("\"qml/LearningBanner.qml\""),
        "not registered in build.rs"
    );
}

#[test]
fn the_controller_exposes_what_the_banner_reads() {
    for property in [
        "learningActive",
        "learningEndsMs",
        "programsSeen",
        "learningIncomplete",
        "droppedCount",
        "learnedNotice",
        "learnedPath",
        "stopLearning",
        "refreshLearningStatus",
    ] {
        assert!(
            CONTROLLER.contains(&format!("cxx_name = \"{property}\"")),
            "SettingsController has no {property}"
        );
    }
}

#[test]
fn the_banner_shows_the_window_and_its_notices_as_plain_text() {
    init_headless_qt_env();

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
        if let Some(mut engine) = engine.as_mut() {
            engine
                .as_mut()
                .load_data(&QByteArray::from(PROBE), &QUrl::from(PROBE_URL));
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "learning banner probe failed to load (QML parse error); stderr:\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| !line.contains(DONE_MARKER))
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "learning banner probe reported errors:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "the learning banner probe never finished; stderr:\n{captured}"
    );
    let _ = app.as_mut();
}

const PROBE: &str = r#"
import QtQuick
import com.snitchwatch.shell

Item {
    id: probe
    width: 800
    height: 600

    QtObject {
        id: stub
        property bool learningActive: false
        property double learningEndsMs: 0
        property int programsSeen: 0
        property bool learningIncomplete: false
        property int droppedCount: 0
        property string learnedNotice: ""
        property string learnedPath: "/home/u/.config/snitchwatch/learned.json"
        property int stops: 0
        property int refreshes: 0
        function stopLearning() { stops++; }
        function refreshLearningStatus() { refreshes++; }
    }

    property int reviews: 0

    LearningBanner {
        id: banner
        settings: stub
        onReviewRequested: probe.reviews++
    }

    function check(ok, what) {
        if (!ok) {
            throw new Error("learning banner: " + what);
        }
    }

    function findAction(item) {
        if (!item) return null;
        if (item.actions !== undefined && item.actions.length > 0) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = findAction(item.children[i]);
            if (hit) return hit;
        }
        return null;
    }

    Component.onCompleted: {
        // No window: not shown.
        check(!banner.shown && !banner.visible, "shown without a window");

        // A window with 5 h 10 min left and 31 programs seen.
        stub.learningEndsMs = banner.nowMs + (5 * 60 + 10) * 60000 - 1000;
        stub.programsSeen = 31;
        stub.learningActive = true;
        check(banner.shown && banner.visible, "not shown for a window");
        check(banner.summary.text === "Ends in 5 h 10 min. 31 programs seen.",
              "summary: " + banner.summary.text);
        stub.programsSeen = 1;
        check(banner.summary.text === "Ends in 5 h 10 min. 1 program seen.",
              "singular: " + banner.summary.text);
        stub.learningEndsMs = banner.nowMs + 25 * 60000 - 1000;
        check(banner.summary.text === "Ends in 25 min. 1 program seen.",
              "minutes: " + banner.summary.text);
        stub.learningEndsMs = banner.nowMs - 5000;
        check(banner.summary.text.indexOf("Ends in 0 min.") === 0,
              "past the end: " + banner.summary.text);

        // The notices follow their properties and are plain text.
        check(!banner.incompleteNotice.visible && !banner.droppedNotice.visible
              && !banner.fileNotice.visible, "notices shown for a clean record");
        stub.learningIncomplete = true;
        stub.droppedCount = 3;
        const markup = "<b>bold</b> <img src='https://example.invalid/x.png'> &amp;";
        stub.learnedNotice = markup;
        stub.learnedPath = markup;
        check(banner.incompleteNotice.visible, "incomplete notice hidden");
        check(banner.incompleteNotice.text.indexOf("missed") >= 0, "incomplete text");
        check(banner.droppedNotice.visible
              && banner.droppedNotice.text.indexOf("3 programs were not recorded") === 0,
              "dropped: " + banner.droppedNotice.text);
        stub.droppedCount = 1;
        check(banner.droppedNotice.text.indexOf("1 program was not recorded") === 0,
              "singular: " + banner.droppedNotice.text);
        stub.droppedCount = 3;
        check(banner.fileNotice.visible && banner.fileNotice.text === markup, "file notice");
        check(banner.whereLabel.text === "Saved in " + markup, "path");
        for (const label of [banner.summary, banner.incompleteNotice, banner.droppedNotice,
                             banner.fileNotice, banner.whereLabel]) {
            check(label.textFormat === Text.PlainText, "a label is " + label.textFormat);
        }

        // Stop calls the controller once; Review only asks the host to open
        // the page. Those are the two actions.
        const message = findAction(banner);
        check(message !== null, "no message with actions");
        check(message.actions.length === 2 && message.actions[0].text === "Stop learning"
              && message.actions[1].text === "Review",
              "actions: " + message.actions.length);
        message.actions[0].trigger();
        check(stub.stops === 1, "Stop was not called once: " + stub.stops);
        message.actions[1].trigger();
        check(probe.reviews === 1, "Review was not requested once: " + reviews);
        check(stub.stops === 1, "Review stopped learning");

        // The window ends: the banner goes away.
        stub.learningActive = false;
        check(!banner.shown && !banner.visible, "still shown after the window");
        console.warn("LEARNING_BANNER_PROBE_DONE");
    }
}
"#;
