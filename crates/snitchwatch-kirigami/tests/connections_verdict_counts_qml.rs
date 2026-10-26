//! Phase 0b: a group header's "last hour" / "today" verdict counts reach
//! QML (`groupLastHour`, `groupToday`) and the real `ConnectionRow` delegate
//! draws them as an icon plus words per verdict. Connection rows report an
//! empty list. A clear followed by the same rows (what a reconnect replays)
//! counts each row once, and `refreshVerdictWindows` (the 60 s tick) leaves
//! fresh figures unchanged. Rows are stamped from `Date.now()`, and the old
//! row just before local midnight, so the windows are deterministic. Any
//! TypeError in the capture fails the test. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_verdict_counts_probe.qml";
const DONE_MARKER: &str = "VERDICT_COUNTS_PROBE_DONE";

#[test]
fn group_headers_show_windowed_verdict_counts_in_words() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000
    height: 600
    property var failures: []
    property int phase: 0
    property int waited: 0

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

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, action, rule, started) {
        return { id: id, process: "p", processPath: null, dstHost: "a.example.com",
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: started,
                 matchedRule: rule };
    }
    function rows() {
        const now = Date.now();
        const midnight = new Date(now);
        midnight.setHours(0, 0, 0, 0);
        return [probe.row("byRule", "allow", "allow-p", now),
                probe.row("byDefault", "allow", null, now),
                probe.row("denied", "deny", null, now),
                probe.row("waiting", null, null, now),
                probe.row("old", "deny", "deny-p",
                          Math.min(midnight.getTime() - 60000, now - 2 * 3600000))];
    }
    function insert() {
        model.applyServerMessageJson(JSON.stringify({
            action: "insertConnectionRows", rows: probe.rows() }));
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
    function at(index) {
        const list = probe.findList(page);
        list.positionViewAtIndex(index, ListView.Contain);
        list.forceLayout();
        return list.itemAtIndex(index);
    }
    function labels(item, out) {
        if (item.objectName === "verdictCount" && item.visible) out.push(item.text);
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) probe.labels(kids[i], out);
        return out;
    }
    readonly property string expected: "2 allowed (1 by default)|1 denied (1 by default)|1 pending"
    function checkHeader(what) {
        const header = probe.at(0);
        probe.expect(header !== null && header.isGroupHeader, what + ": no program header");
        if (!header) return;
        const hour = JSON.parse(header.groupLastHour);
        const today = JSON.parse(header.groupToday);
        probe.expect(hour.map(s => s.kind).join(",") === "allowed,denied,pending",
                     what + ": hour kinds " + header.groupLastHour);
        probe.expect(today.map(s => s.text).join("|") === probe.expected,
                     what + ": today " + header.groupToday);
        const shown = probe.labels(header, []).join("|");
        probe.expect(shown === probe.expected + "|" + probe.expected,
                     what + ": delegate text '" + shown + "'");
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const list = probe.findList(page);
                if (probe.phase > 0 && !(list && list.count >= 1 && probe.at(0))) {
                    if (++probe.waited < 100) return;
                    throw new Error("timed out waiting for the program header");
                }
                probe.waited = 0;
                if (probe.phase === 0) {
                    model.setGroupedMode(true);
                    model.setPendingOnly(false);
                    probe.insert();
                } else if (probe.phase === 1) {
                    probe.checkHeader("first load");
                    model.refreshVerdictWindows();
                    probe.checkHeader("after the tick");
                    model.toggleProcessGroup("p");
                } else if (probe.phase === 2) {
                    probe.expect(list.count >= 2, "domain row after expanding: " + list.count);
                    model.toggleDomainGroup("p", "example.com");
                } else if (probe.phase === 3) {
                    model.toggleHostGroup("p", "example.com", "a.example.com");
                } else if (probe.phase === 4) {
                    let leaf = null;
                    for (let i = 0; i < list.count; ++i) {
                        const item = probe.at(i);
                        if (item && !item.isGroupHeader) { leaf = item; break; }
                    }
                    probe.expect(leaf !== null, "no connection row in " + list.count + " rows");
                    if (leaf) {
                        probe.expect(leaf.groupLastHour === "[]" && leaf.groupToday === "[]",
                                     "connection row windows: " + leaf.groupLastHour + " / " + leaf.groupToday);
                    }
                    model.applyServerMessageJson(JSON.stringify({ action: "clearConnectionRows" }));
                    probe.insert();
                } else if (probe.phase === 5) {
                    probe.checkHeader("after clear and replay");
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
                        throw new Error("verdict counts probe: " + probe.failures.join("; "));
                    }
                    console.warn("VERDICT_COUNTS_PROBE_DONE");
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
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "verdict counts probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
