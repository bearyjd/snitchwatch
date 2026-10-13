//! The Connections header's filter controls drive the model: typing
//! `host:github port:443`, "Denied only", "From now", and the
//! `setFilterQuery("country:XX")` path a map click will use. A probe window
//! checks the visible rows after each; failures are thrown against the
//! probe's URL, which the stderr capture turns into an assertion. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_filters_probe.qml";
const DONE_MARKER: &str = "FILTERS_PROBE_DONE";

#[test]
fn header_filters_drive_the_visible_rows() {
    init_headless_qt_env();
    // Demo mode must be off for the "inert" assertion below.
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 900
    height: 600
    property var failures: []
    property int phase: 0
    property int resets: 0
    readonly property real future: Date.now() + 3600000

    ConnectionsModel { id: model }
    ConnectionsHeader {
        id: hdr
        width: parent.width
        model: model
        title: "Connections"
    }
    ConnectionsFilterBar {
        id: bar
        y: 100
        width: parent.width
        model: model
    }
    Connections {
        target: model
        function onModelReset() { probe.resets++; }
    }

    function row(id, process, host, port, action, started) {
        return { id: id, process: process, processPath: null, dstHost: host,
                 dstIp: "8.8.8.8", dstPort: port, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: started };
    }
    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function find(item, pred) {
        if (pred(item)) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.find(kids[i], pred);
            if (f) return f;
        }
        return null;
    }
    function switchWith(prefix) {
        return probe.find(bar, it => typeof it.text === "string" && typeof it.click === "function"
                                     && it.text.indexOf(prefix) === 0);
    }
    function searchField() {
        return probe.find(hdr, it => it.placeholderText !== undefined && it.text !== undefined);
    }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const search = probe.searchField();
                if (probe.phase === 0) {
                    probe.expect(search !== null, "no search field in the header");
                    model.setGroupedMode(false);
                } else if (probe.phase === 1) {
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("g443", "firefox", "github.com", 443, "allow", probe.future),
                               probe.row("g80", "firefox", "github.com", 80, "allow", probe.future),
                               probe.row("s443", "slack", "slack.com", 443, "allow", probe.future),
                               probe.row("old443", "firefox", "github.com", 443, "allow", 1),
                               probe.row("den", "curl", "x.example", 8080, "deny", probe.future)]
                    }));
                } else if (probe.phase === 2) {
                    probe.expect(model.count === 5, "all rows before filtering: " + model.count);
                    probe.expect(!model.filterActive && model.filterSummary === "", "filter active at start");
                    search.text = "host:github port:443";
                    probe.expect(model.count === 2, "host:github port:443 shows g443 and old443: " + model.count);
                    probe.expect(model.filterActive, "filterActive after typing");
                    probe.expect(model.filterText === "host:github port:443", "filterText: " + model.filterText);
                    // From now hides the row that started before it was turned on.
                    const fromNow = probe.switchWith("From now");
                    probe.expect(fromNow !== null, "no From now switch");
                    fromNow.click();
                    probe.expect(model.filterSinceMs > 0, "From now did not set a start");
                    probe.expect(model.count === 1, "From now hides the older row: " + model.count);
                    probe.expect(fromNow.checked && model.filterSummary.indexOf("even if still active") >= 0,
                                 "From now summary does not explain its semantics: " + model.filterSummary);
                    probe.expect(probe.switchWith("From now (since ") !== null, "From now does not show its start time");
                    probe.switchWith("From now").click();
                    probe.expect(model.filterSinceMs === 0, "From now off did not clear the start");
                    probe.expect(model.count === 2, "From now off restores the older row: " + model.count);
                    search.text = "";
                    probe.expect(model.count === 5 && !model.filterActive, "clearing restores all: " + model.count);
                    // Denied only.
                    const denied = probe.switchWith("Denied only");
                    probe.expect(denied !== null, "no Denied only switch");
                    denied.click();
                    probe.expect(model.count === 1, "Denied only shows the denied row: " + model.count);
                    probe.expect(model.filterDeniedOnly && denied.checked, "Denied only switch out of step with the model");
                    // Both on: nothing can match, and the summary says why.
                    const pending = probe.switchWith("Pending only");
                    pending.click();
                    probe.expect(model.count === 0, "pending and denied together match nothing: " + model.count);
                    probe.expect(model.filterSummary.indexOf("cannot both match") >= 0, "no explanation: " + model.filterSummary);
                    pending.click();
                    probe.expect(!pending.checked && !model.filterPendingOnly, "Pending only did not turn off");
                    // A change from outside moves the switch.
                    model.setPendingOnly(true);
                    probe.expect(pending.checked, "Pending only switch did not follow the model");
                    model.setPendingOnly(false);
                    probe.expect(!pending.checked, "Pending only switch did not follow the model off");
                    denied.click();
                    probe.expect(model.count === 5, "Denied only off: " + model.count);
                    // The map-click path: set from outside, the field follows.
                    model.setFilterQuery("country:DE");
                    probe.expect(search.text === "country:DE", "search field did not follow setFilterQuery: " + search.text);
                    probe.expect(model.filterActive, "country filter is not active");
                    if (model.filterSummary.indexOf("no GeoIP database") >= 0) {
                        probe.expect(model.count === 0, "country: without a database must match nothing: " + model.count);
                    } else {
                        probe.expect(model.count < 5, "country:DE did not narrow the list: " + model.count);
                    }
                    model.setFilterQuery("");
                    probe.expect(search.text === "" && model.count === 5, "clearing the query: " + model.count);
                    model.setFilterQuery("port:abc");
                    probe.expect(model.count === 0 && model.filterSummary.indexOf("not a port number") >= 0,
                                 "port:abc: " + model.count + " / " + model.filterSummary);
                    model.setFilterQuery("");
                    // Grouped mode filters with ranged signals, never a reset.
                    model.setGroupedMode(true);
                } else if (probe.phase === 3) {
                    probe.resets = 0;
                    const base = model.count;
                    probe.expect(base > 1, "grouped baseline: " + base);
                    model.setFilterQuery("host:github");
                    probe.expect(model.count !== base, "a grouped query did not change the rows: " + model.count + " of " + base);
                    const queried = model.count;
                    model.setFromNow(true);
                    probe.expect(model.filterSinceMs > 0, "grouped From now did not set a start");
                    model.setDeniedOnly(true);
                    probe.expect(model.count !== queried, "grouped denied-only did not change the rows: " + model.count + " of " + queried);
                    model.setDeniedOnly(false);
                    model.setFromNow(false);
                    model.setFilterQuery("");
                    probe.expect(!model.filterActive, "clearing left a filter active");
                    probe.expect(probe.resets === 0, "a grouped filter change reset the model: " + probe.resets);
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
                        throw new Error("filters probe: " + probe.failures.join("; "));
                    }
                    console.warn("FILTERS_PROBE_DONE");
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
    assert!(bad.is_empty(), "filters probe failed:\n{}", bad.join("\n"));
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
