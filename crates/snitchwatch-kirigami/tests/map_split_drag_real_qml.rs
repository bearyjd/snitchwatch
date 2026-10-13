//! A REAL mouse drag of the real `MonitorPane` splitter handle (QtTest
//! `mouseDrag` on the real `ConnectionsPage`): a collapsed map band reopens when
//! the handle is dragged down, stays within `chromeHeight + maximumBody`, and
//! dragging the handle up to zero collapses it again. Guards the height cap in
//! `MonitorPane.qml` (a cap of `band.maximumHeight` alone pins a collapsed band
//! at its header). Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::{bridge_bindings as _, connections_model as _, geo_model as _};

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/map_split_drag_real_probe.qml";
const DONE_MARKER: &str = "MAP_SPLIT_DRAG_PROBE_DONE";

#[test]
fn dragging_the_real_handle_reopens_and_collapses_the_map_band() {
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
    width: 900
    height: 800
    property var failures: []
    property int phase: 0

    QtObject {
        id: stubGeo
        property bool dbAvailable: true
        signal modelReset()
        signal countryActivity(string code)
        function landPath() { return "M0 0 L3600 0 L3600 1800 L0 1800 Z"; }
        function clustersJson(scale, radius) { return "[]"; }
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        geoModel: stubGeo
        model: ConnectionsModel { id: model }
    }
    TestCase { id: tc; name: "mapsplit"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function findHandle(item) {
        if (item.Accessible && item.Accessible.name === "Resize map") return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.findHandle(kids[i]);
            if (f) return f;
        }
        return null;
    }
    function drag(handle, dy) {
        const p = page.mapFromItem(handle, handle.width / 2, handle.height / 2);
        tc.mouseDrag(page, p.x, p.y, 0, dy, Qt.LeftButton);
    }

    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const band = page.mapBand;
                if (probe.phase === 1) {
                    MapBandState.collapsed = true;
                } else if (probe.phase === 3) {
                    const handle = probe.findHandle(page);
                    probe.expect(handle !== null, "no handle named 'Resize map'");
                    probe.expect(MapBandState.collapsed && band.height <= band.collapsedHeight + 1,
                                 "band not collapsed: " + band.height);
                    if (handle) probe.drag(handle, 200);
                    probe.expect(!MapBandState.collapsed, "dragging a collapsed band down did not reopen it");
                    probe.expect(band.height > band.collapsedHeight + band.minimumBody / 2,
                                 "band stayed small: " + band.height);
                    probe.expect(band.height <= band.chromeHeight + band.maximumBody + 1,
                                 "band over its maximum: " + band.height);
                } else if (probe.phase === 5) {
                    const handle = probe.findHandle(page);
                    if (handle) probe.drag(handle, -1000);
                    probe.expect(MapBandState.collapsed, "dragging to zero did not collapse the band");
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
                        throw new Error("map split drag probe: " + probe.failures.join("; "));
                    }
                    console.warn("MAP_SPLIT_DRAG_PROBE_DONE");
                } finally {
                    MapBandState.collapsed = false;
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
        "the real splitter drag misbehaved:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
