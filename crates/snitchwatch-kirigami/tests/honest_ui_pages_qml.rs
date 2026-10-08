//! Integration smoke: the "not enforced" banners on `BlocklistsPage.qml` and
//! `ProfilesPage.qml` (issues #45/#46) are live, visible and non-dismissable
//! once the real pages are instantiated, and the edited `RulesPage.qml` /
//! `ConnectionsPage.qml` (-> `PendingDecisionSheet.qml`) still load.
//!
//! Same two-layer shape as `inline_verdict_qml.rs`: a real `Window` driven by
//! a real event loop (so delegates and header bindings actually evaluate), with
//! stderr captured so a QML JS error — including the probe `Timer`'s own
//! `throw`s, which Qt reports prefixed with the probe's URL — fails the test,
//! plus a separate null-root check for QML parse errors. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/honest_ui_pages_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn preview_banners_are_visible_and_not_dismissable() {
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
    visible: true
    width: 1200
    height: 600

    BlocklistsPage {
        id: blocklistsPage
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: BlocklistsModel {}
        entriesModel: BlocklistEntriesModel {}
    }
    ProfilesPage {
        id: profilesPage
        anchors { left: blocklistsPage.right; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: ProfilesModel {}
    }
    RulesPage {
        id: rulesPage
        // Opens the inspector sheet (and its PlainText header override) on a
        // rule whose name looks like markup, so a broken header surfaces as a
        // page warning below.
        Component.onCompleted: openRuleByName("<b>bold</b>-rule")
        anchors { left: profilesPage.right; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: RulesModel {
            Component.onCompleted: applyServerMessageJson(JSON.stringify({
                action: "setRules",
                rules: [
                    { name: "<b>bold</b>-rule", enabled: true, action: "allow",
                      duration: "always", description: "",
                      operator: { operand: "process.path", data: "<i>x</i>" } }
                ]
            }))
        }
    }
    ConnectionsPage {
        anchors { right: parent.right; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: ConnectionsModel {}
    }

    function checkBanner(page, name) {
        const banner = page.header;
        if (!banner || banner.visible !== true) {
            throw new Error(name + ": not-enforced banner is not visible");
        }
        if (banner.type !== Kirigami.MessageType.Warning) {
            throw new Error(name + ": banner is not a Warning");
        }
        if (banner.showCloseButton) {
            throw new Error(name + ": banner is dismissable");
        }
        // Laid out for real: full page width and a non-zero height, not a
        // zero-sized item that merely reports `visible`.
        if (banner.width < page.width - 1 || banner.height <= 0) {
            throw new Error(name + ": banner not laid out: " + banner.width + "x" + banner.height
                            + " in a page " + page.width + " wide");
        }
        if (banner.text.indexOf("not applied") < 0) {
            throw new Error(name + ": banner text does not say it is not applied: " + banner.text);
        }
    }

    // Quits the loop once layout/polish has run; `finally` keeps Qt.quit()
    // reachable when a check throws, which would otherwise hang the binary.
    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                checkBanner(blocklistsPage, "BlocklistsPage");
                checkBanner(profilesPage, "ProfilesPage");
                if (rulesPage.inspectName !== "<b>bold</b>-rule") {
                    throw new Error("RulesPage inspector did not open on the markup-named rule");
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
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "honest-UI probe failed: root object was null — a QML parse error (syntax error, \
         unregistered/misspelled type) in the probe or one of the edited pages"
    );

    // The probe's own URL (its Timer `throw`s) plus any warning raised inside
    // one of this shell's pages. An unfiltered scan would trip on the upstream
    // Kirigami OverlaySheet binding-loop warning every sheet emits.
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) or warning(s) reported against the probe URL or one of the shell's \
         pages — a broken page binding / anchor loop, or the probe Timer's own banner \
         assertions (visible / Warning / no close button / laid out / says \"not applied\"). \
         Captured stderr:\n{}",
        bad_lines.join("\n")
    );
}
