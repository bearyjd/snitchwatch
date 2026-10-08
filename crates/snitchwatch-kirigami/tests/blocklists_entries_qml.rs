//! Issue #67: the Blocklists page's entry list, through the real models.
//!
//! - opening a list asks for its first page with this GUI's request id, the
//!   same on every request;
//! - a page answering another GUI's request is ignored;
//! - a later page from a newer download empties the list (never old and new
//!   hosts together) and the page asks for the list again from the start,
//!   once;
//! - the first page of the new download, then the rest, fill the list.
//!
//! Same probe shape as `blocklists_leftovers_qml.rs`. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/blocklists_entries_probe.qml";

#[test]
fn entry_pages_are_kept_by_request_and_restart_when_the_list_changes() {
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
    function pageOf(requestId, download, offset, hosts) {
        entries.applyServerMessageJson(JSON.stringify({
            action: "setBlocklistEntries",
            subscriptionId: "l1",
            entries: hosts.map(function (h) { return { host: h }; }),
            offset: offset,
            total: 4,
            requestId: requestId,
            lastUpdatedIso8601: download
        }));
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
                model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists",
                    blocklists: [
                        { id: "l1", displayName: "Ads", url: "https://example.invalid/ads.txt",
                          entryCount: 4, status: "ok", enforcement: "not_enforced" }
                    ],
                    storage: { persistent: true }
                }));
                page.openInspector({
                    listId: "l1", displayName: "Ads", url: "https://example.invalid/ads.txt",
                    entryCount: 4, status: "ok", statusLabel: "Downloaded", enforcementLabel: "",
                    enforcementReason: "", lastUpdated: "", lastFailureReason: ""
                });
                probeWindow.check(probeWindow.sent.length === 1
                                  && probeWindow.sent[0].action === "requestBlocklistEntries"
                                  && probeWindow.sent[0].subscriptionId === "l1"
                                  && probeWindow.sent[0].offset === 0,
                                  "first request: " + JSON.stringify(probeWindow.sent));
                const mine = probeWindow.sent[0].requestId;
                probeWindow.check(typeof mine === "string" && mine.length > 0 && mine.length <= 64,
                                  "no request id: " + mine);

                // Another GUI's page, broadcast to this one too.
                probeWindow.pageOf("someone-else", "t1", 0, ["x.example", "y.example"]);
                probeWindow.check(entries.count === 0, "kept another GUI's page: " + entries.count);

                probeWindow.pageOf(mine, "t1", 0, ["a.example", "b.example"]);
                probeWindow.check(entries.count === 2 && entries.hasMore, "own first page: " + entries.count);

                // "Show more" after the list was downloaded again.
                probeWindow.pageOf(mine, "t2", 2, ["n3.example", "n4.example"]);
                probeWindow.check(entries.count === 0, "mixed old and new hosts: " + entries.count);
                probeWindow.check(probeWindow.sent.length === 2
                                  && probeWindow.sent[1].action === "requestBlocklistEntries"
                                  && probeWindow.sent[1].subscriptionId === "l1"
                                  && probeWindow.sent[1].offset === 0
                                  && probeWindow.sent[1].requestId === mine,
                                  "no restart request: " + JSON.stringify(probeWindow.sent));

                probeWindow.pageOf(mine, "t2", 0, ["n1.example", "n2.example"]);
                probeWindow.pageOf(mine, "t2", 2, ["n3.example", "n4.example"]);
                probeWindow.check(entries.count === 4 && !entries.hasMore, "restarted list: " + entries.count);
                probeWindow.check(probeWindow.sent.length === 2, "asked again without need");

                if (probeWindow.failures.length > 0) {
                    throw new Error("entries probe: " + probeWindow.failures.join("; "));
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
        "BlocklistsPage entries probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "BlocklistsPage entries probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
