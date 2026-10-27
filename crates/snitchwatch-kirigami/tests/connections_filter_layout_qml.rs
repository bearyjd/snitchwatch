//! The filter switches and the summary must sit above the list, not over it:
//! the first list row has to start at or below the bottom of the filter bar,
//! with the bar's Flow wrapped (narrow window), its summary showing, and
//! the exception-first rows (BackgroundGroupBar) showing. The
//! bar was once part of the page's fixed-height title row and covered the
//! first rows. Drives the real `ConnectionsPage`; geometry is compared in page
//! coordinates. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_filter_layout_probe.qml";
const DONE_MARKER: &str = "FILTER_LAYOUT_PROBE_DONE";

#[test]
fn the_first_list_row_starts_below_the_filter_bar() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 360
    height: 600
    property var failures: []
    property int phase: 0
    property real barHeightBefore: 0

    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {}
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
    }
    TestCase { id: tc; name: "filterlayout"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host) {
        return { id: id, process: "p", processPath: null, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function findList(item) {
        if (typeof item.itemAtIndex === "function" && item.currentIndex !== undefined) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.findList(kids[i]);
            if (f) return f;
        }
        return null;
    }

    function check(label) {
        const list = probe.findList(page);
        probe.expect(list !== null, label + ": no ListView found under the page");
        const bar = page.header;
        probe.expect(bar !== null && bar !== undefined, label + ": the page has no header item");
        if (!list || !bar) return;
        const first = list.itemAtIndex(0);
        probe.expect(first !== null, label + ": no delegate at index 0");
        if (!first) return;
        const barBottom = bar.mapToItem(page, 0, 0).y + bar.height;
        const firstTop = first.mapToItem(page, 0, 0).y;
        probe.expect(bar.height > 0, label + ": filter bar has no height");
        probe.expect(firstTop >= barBottom,
                     label + ": first row y " + firstTop + " is above the filter bar bottom " + barBottom);
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    model.setGroupedMode(false);
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com"), probe.row("b", "y.example.com")]
                    }));
                } else if (probe.phase === 2) {
                    probe.check("no filter");
                    // Narrow window, every option on: the Flow wraps and the
                    // summary shows, so the bar is as tall as it gets.
                    probe.barHeightBefore = page.header.height;
                    model.setFilterQuery("example");
                    model.setFromNow(true);
                    model.setFromNow(false);
                    model.setPendingOnly(true);
                    model.setPendingOnly(false);
                    model.setDeniedOnly(true);
                    model.setDeniedOnly(false);
                    model.setFilterQuery("port:443 host:example");
                } else if (probe.phase === 4) {
                    probe.expect(page.header.height > probe.barHeightBefore,
                                 "the summary did not grow the bar: " + page.header.height);
                    probe.check("summary showing");
                    // The exception-first rows (0c): Showing N of M, the
                    // collapsed group and the baseline notice, each with a
                    // button, wrap at this width and must still push the list.
                    model.setFilterQuery("");
                    probe.barHeightBefore = page.header.height;
                    const handled = probe.row("h", "ntp.example.com");
                    handled.processPath = "/usr/sbin/chronyd";
                    handled.matchedRule = "snitchwatch-default-chronyd";
                    model.applyServerMessageJson(JSON.stringify({ action: "clearConnectionRows" }));
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com"), probe.row("b", "y.example.com"), handled]
                    }));
                    model.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: [
                        { name: "snitchwatch-default-chronyd", enabled: true, action: "allow", toggleable: true }] }));
                } else if (probe.phase === 6) {
                    probe.expect(model.backgroundCount === 1 && model.baselineNotice !== "",
                                 "the background group is not showing: " + model.backgroundCount);
                    probe.expect(page.header.height > probe.barHeightBefore,
                                 "the background rows did not grow the bar: " + page.header.height);
                    probe.check("background group showing");
                    done = true;
                }
                probe.phase++;
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("filter layout probe: " + probe.failures.join("; "));
                    }
                    console.warn("FILTER_LAYOUT_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
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
        "probe root was null:\n{captured}"
    );

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "the filter bar overlaps the list:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
