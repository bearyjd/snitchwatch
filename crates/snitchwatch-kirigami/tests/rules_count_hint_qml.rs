//! Issue #65 (option c): the Rules page says, in one fixed plain-text
//! sentence pair, that the firewall service reports a different number of
//! rules than the list shows, when the bridge's `RulesNotShown` says so
//! (`countMismatch`). It is advice: it hides when the next `RulesNotShown`
//! has none, a list alone neither shows nor hides it, and no daemon text is
//! rendered.
//!
//! Same probe shape as `rules_all_apps_qml.rs`. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_count_hint_probe.qml";

#[test]
fn the_count_hint_follows_the_bridges_flag_in_one_plain_label() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 1000
    height: 800

    property var failures: []

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function notShown(fields) {
        rulesModel.applyServerMessageJson(JSON.stringify(
            Object.assign({ action: "rulesNotShown", tooLarge: 0, listed: true }, fields)));
    }
    function setRules(names) {
        rulesModel.applyServerMessageJson(JSON.stringify({
            action: "setRules",
            rules: names.map(function (name) {
                return { name: name, displayName: name, enabled: true, action: "allow",
                         duration: "always", description: "", precedence: false, nolog: false,
                         operator: { type: "simple", operand: "dest.host", data: "x.example",
                                     sensitive: false },
                         readOnlyReason: null };
            }) }));
    }
    function findChild(item, name) {
        if (!item) {
            return null;
        }
        if (item.objectName === name) {
            return item;
        }
        const kids = item.children || [];
        for (let i = 0; i < kids.length; i++) {
            const found = probeWindow.findChild(kids[i], name);
            if (found) {
                return found;
            }
        }
        return item.contentItem ? probeWindow.findChild(item.contentItem, name) : null;
    }

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const hint = probeWindow.findChild(page.header, "rulesCountHint");
                probeWindow.check(hint !== null, "the hint label exists");
                const other = probeWindow.findChild(page.header, "rulesNotShown");

                // Nothing yet, then a list with nothing to say.
                probeWindow.check(!hint.visible && !page.header.visible, "hidden at the start");
                probeWindow.setRules(["100-a", "100-b"]);
                probeWindow.notShown({});
                probeWindow.check(!hint.visible && hint.text === "" && !page.header.visible,
                                  "no mismatch, no hint: " + hint.text);

                // The bridge says the counts differ.
                probeWindow.notShown({ countMismatch: true });
                const text = rulesModel.countHintText;
                probeWindow.check(hint.visible && page.header.visible,
                                  "the hint shows with the header");
                probeWindow.check(hint.textFormat === Text.PlainText, "plain text");
                probeWindow.check(hint.text === text && text.length > 0
                                  && text.indexOf("different number of rules") >= 0
                                  && text.indexOf("changed outside Snitchwatch") >= 0
                                  && text.indexOf("Restarting the firewall service") >= 0,
                                  "the fixed sentence: " + hint.text);
                probeWindow.check(other && !other.visible,
                                  "it is not the not-shown label's text");

                // A list alone neither shows nor hides it (it rides RulesNotShown).
                probeWindow.setRules(["100-a", "100-b", "100-c"]);
                probeWindow.check(hint.visible, "a list alone leaves the hint");
                // The next RulesNotShown with the flag keeps it, without flicker.
                probeWindow.notShown({ countMismatch: true });
                probeWindow.check(hint.visible && hint.text === text, "it holds");

                // Next to what the list leaves out, as its own label.
                probeWindow.notShown({ tooLarge: 1, countMismatch: true });
                probeWindow.check(hint.visible && other.visible
                                  && other.text.indexOf("1 rule isn't listed") === 0
                                  && other.text.indexOf("different number") < 0,
                                  "two labels: " + other.text);

                // One without the flag (the bridge cleared it, or an older one) hides it.
                probeWindow.notShown({});
                probeWindow.check(!hint.visible && hint.text === "" && !page.header.visible,
                                  "cleared by the next message");

                // With no list there is nothing to differ from, whatever a frame says.
                probeWindow.notShown({ listed: false, countMismatch: true });
                probeWindow.check(!hint.visible && rulesModel.countHintText === "",
                                  "no list, no hint");

                if (probeWindow.failures.length > 0) {
                    throw new Error("count hint probe: " + probeWindow.failures.join("; "));
                }
            } finally {
                Qt.quit();
            }
        }
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
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
        "RulesPage count-hint probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage count-hint probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
