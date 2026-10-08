//! Issue #73: the Blocklists page offers to remove blocklist rules Snitchwatch
//! made that nothing manages, and asks first.
//!
//! - nothing shows for a count of 0;
//! - a count shows in a PlainText label (singular and plural);
//! - "Remove these rules" asks for confirmation; Cancel sends nothing; Confirm
//!   sends exactly one `removeLeftoverBlocklistRules`;
//! - the count going back to 0 hides the notice.
//!
//! Same probe shape as `rules_hits_qml.rs`. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/blocklists_leftovers_probe.qml";

#[test]
fn leftover_rules_are_offered_for_removal_after_a_confirmation() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 1000
    height: 800

    property var failures: []
    property var sent: []

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function leftovers(count) {
        model.applyServerMessageJson(JSON.stringify({ action: "setBlocklistLeftovers", count: count }));
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
    function part(name) {
        return probeWindow.findChild(page.header, name);
    }

    BlocklistsModel { id: model }
    BlocklistEntriesModel { id: entries }
    BlocklistsPage {
        id: page
        anchors.fill: parent
        model: model
        entriesModel: entries
    }

    Connections {
        target: model
        function onSubscriptionRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const notice = probeWindow.part("leftoverNotice");
                probeWindow.check(notice !== null, "no leftover notice in the page header");
                probeWindow.check(notice && !notice.visible, "a notice for zero leftovers");

                probeWindow.leftovers(1);
                probeWindow.check(notice.visible, "no notice for one leftover rule");
                const text = probeWindow.part("leftoverText");
                probeWindow.check(text.text.startsWith("1 blocklist rule made by Snitchwatch is "
                                                       + "still in the firewall, but this service "
                                                       + "isn't managing it"),
                                  "singular text: '" + text.text + "'");
                probeWindow.leftovers(3);
                probeWindow.check(probeWindow.part("leftoverText").text.startsWith(
                                  "3 blocklist rules made by Snitchwatch are still"),
                                  "plural text: '" + probeWindow.part("leftoverText").text + "'");
                probeWindow.check(text.textFormat === Text.PlainText, "the count is not PlainText");

                // Asks first; cancelling sends nothing.
                const remove = probeWindow.part("removeLeftovers");
                probeWindow.check(remove.visible, "no remove button");
                remove.clicked();
                probeWindow.check(page.confirmingLeftover, "no confirmation step");
                probeWindow.check(probeWindow.sent.length === 0, "removed before confirming");
                probeWindow.part("cancelRemoveLeftovers").clicked();
                probeWindow.check(!page.confirmingLeftover && probeWindow.sent.length === 0,
                                  "cancel sent something: " + JSON.stringify(probeWindow.sent));

                // Confirming sends exactly one message.
                remove.clicked();
                probeWindow.part("confirmRemoveLeftovers").clicked();
                probeWindow.check(probeWindow.sent.length === 1
                                  && probeWindow.sent[0].action === "removeLeftoverBlocklistRules",
                                  "confirm sent: " + JSON.stringify(probeWindow.sent));
                probeWindow.check(!page.confirmingLeftover, "still asking after confirming");

                // The bridge reports none left.
                probeWindow.leftovers(0);
                probeWindow.check(!notice.visible, "the notice stayed at zero");

                if (probeWindow.failures.length > 0) {
                    throw new Error("leftovers probe: " + probeWindow.failures.join("; "));
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
        "BlocklistsPage leftovers probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "BlocklistsPage leftovers probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
